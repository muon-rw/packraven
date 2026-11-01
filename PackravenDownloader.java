import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.HttpURLConnection;
import java.net.URL;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.nio.file.StandardCopyOption;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.HashSet;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.Future;
import java.util.concurrent.atomic.AtomicInteger;

// packraven compiles this with --release 8, so it runs on any Java a Minecraft server can, JREs included.
// Rebuild the jar next to it with `npm run build:downloader` after you change it.
public final class PackravenDownloader {
    private static final Path MANIFEST = Paths.get("packraven-downloads.txt");
    // We remember what we installed so the next pack version can remove the files it dropped, and so
    // a normal start doesn't hash every file again.
    private static final Path INSTALLED = Paths.get(".packraven-installed.txt");
    private static final int THREADS = 8;
    private static final int ATTEMPTS = 3;

    private static final class Download {
        final String sha1;
        final long size;
        final String url;
        final String path;

        Download(String sha1, long size, String url, String path) {
            this.sha1 = sha1;
            this.size = size;
            this.url = url;
            this.path = path;
        }
    }

    public static void main(String[] args) throws Exception {
        List<Download> downloads = readManifest();
        Map<String, String> installed = readInstalled();
        removeDropped(downloads, installed);

        List<Download> missing = new ArrayList<>();
        for (Download download : downloads) {
            if (!present(download, installed)) {
                missing.add(download);
            }
        }
        Map<String, String> failures = new LinkedHashMap<>();
        if (!missing.isEmpty()) {
            System.out.println("Downloading " + missing.size() + " of the pack's " + downloads.size() + " mod files");
            failures = downloadAll(missing);
        }
        writeInstalled(downloads, failures.keySet());
        if (!failures.isEmpty()) {
            System.out.println("Couldn't download " + (failures.size() == 1 ? "1 file:" : failures.size() + " files:"));
            for (Map.Entry<String, String> failure : failures.entrySet()) {
                System.out.println("  " + failure.getKey() + ": " + failure.getValue());
            }
            System.out.println("Check your internet connection, then run this script again. It only downloads the files that are still missing.");
            System.exit(1);
        }
    }

    private static List<Download> readManifest() throws IOException {
        List<Download> downloads = new ArrayList<>();
        for (String line : Files.readAllLines(MANIFEST, StandardCharsets.UTF_8)) {
            if (line.isEmpty() || line.startsWith("#")) {
                continue;
            }
            String[] fields = line.split("\t", 4);
            if (fields.length != 4 || !safePath(fields[3])) {
                throw new IOException(MANIFEST + " has a malformed line: " + line);
            }
            downloads.add(new Download(fields[0].toLowerCase(), Long.parseLong(fields[1]), fields[2], fields[3]));
        }
        return downloads;
    }

    private static boolean safePath(String path) {
        if (path.isEmpty() || path.startsWith("/") || path.contains("\\") || path.contains(":")) {
            return false;
        }
        for (String part : path.split("/")) {
            if (part.isEmpty() || part.equals(".") || part.equals("..")) {
                return false;
            }
        }
        return true;
    }

    private static Map<String, String> readInstalled() throws IOException {
        Map<String, String> installed = new HashMap<>();
        if (!Files.exists(INSTALLED)) {
            return installed;
        }
        for (String line : Files.readAllLines(INSTALLED, StandardCharsets.UTF_8)) {
            String[] fields = line.split("\t", 2);
            if (fields.length == 2 && safePath(fields[1])) {
                installed.put(fields[1], fields[0]);
            }
        }
        return installed;
    }

    // We only remove a file the pack no longer lists if it's still the one we downloaded, so a jar
    // you put at the same path yourself stays.
    private static void removeDropped(List<Download> downloads, Map<String, String> installed) throws IOException {
        Set<String> wanted = new HashSet<>();
        for (Download download : downloads) {
            wanted.add(download.path);
        }
        for (Map.Entry<String, String> entry : installed.entrySet()) {
            Path path = Paths.get(entry.getKey());
            if (!wanted.contains(entry.getKey()) && Files.isRegularFile(path) && entry.getValue().equals(sha1(path))) {
                Files.delete(path);
                System.out.println("Removed " + entry.getKey() + ", which this version of the pack dropped");
            }
        }
    }

    // A server admin who renames a jar to .disabled wants it off, so we don't download it again.
    private static boolean present(Download download, Map<String, String> installed) throws IOException {
        Path path = Paths.get(download.path);
        if (Files.exists(Paths.get(download.path + ".disabled"))) {
            return true;
        }
        if (!Files.isRegularFile(path) || Files.size(path) != download.size) {
            return false;
        }
        return download.sha1.equals(installed.get(download.path)) || download.sha1.equals(sha1(path));
    }

    private static Map<String, String> downloadAll(List<Download> downloads) throws InterruptedException {
        ExecutorService pool = Executors.newFixedThreadPool(THREADS);
        AtomicInteger done = new AtomicInteger();
        Map<Download, Future<String>> results = new LinkedHashMap<>();
        for (final Download download : downloads) {
            results.put(download, pool.submit(() -> {
                String failure = downloadWithRetries(download);
                if (failure == null) {
                    System.out.println("[" + done.incrementAndGet() + "/" + downloads.size() + "] " + download.path);
                }
                return failure;
            }));
        }
        pool.shutdown();
        Map<String, String> failures = new LinkedHashMap<>();
        for (Map.Entry<Download, Future<String>> result : results.entrySet()) {
            try {
                String failure = result.getValue().get();
                if (failure != null) {
                    failures.put(result.getKey().path, failure);
                }
            } catch (ExecutionException e) {
                failures.put(result.getKey().path, String.valueOf(e.getCause()));
            }
        }
        return failures;
    }

    private static String downloadWithRetries(Download download) {
        String failure = null;
        for (int attempt = 1; attempt <= ATTEMPTS; attempt++) {
            try {
                download(download);
                return null;
            } catch (IOException e) {
                failure = e.getMessage();
            }
        }
        return failure;
    }

    private static void download(Download download) throws IOException {
        Path target = Paths.get(download.path);
        Path part = Paths.get(download.path + ".part");
        if (target.getParent() != null) {
            Files.createDirectories(target.getParent());
        }
        HttpURLConnection connection = (HttpURLConnection) new URL(download.url).openConnection();
        connection.setRequestProperty("User-Agent", "packraven-server-downloader");
        connection.setConnectTimeout(15_000);
        connection.setReadTimeout(60_000);
        int status = connection.getResponseCode();
        if (status != 200) {
            throw new IOException(download.url + " answered HTTP " + status);
        }
        MessageDigest digest = sha1Digest();
        try (InputStream in = connection.getInputStream(); OutputStream out = Files.newOutputStream(part)) {
            byte[] buffer = new byte[1 << 16];
            int read;
            while ((read = in.read(buffer)) > 0) {
                digest.update(buffer, 0, read);
                out.write(buffer, 0, read);
            }
        } catch (IOException e) {
            Files.deleteIfExists(part);
            throw e;
        }
        String actual = hex(digest.digest());
        if (!actual.equals(download.sha1)) {
            Files.deleteIfExists(part);
            throw new IOException(download.url + " sent a file with SHA-1 " + actual + " instead of " + download.sha1);
        }
        Files.move(part, target, StandardCopyOption.REPLACE_EXISTING);
    }

    // Every file that didn't fail is now verified: present() checked its hash or we just downloaded it.
    private static void writeInstalled(List<Download> downloads, Set<String> failed) throws IOException {
        List<String> lines = new ArrayList<>();
        for (Download download : downloads) {
            if (!failed.contains(download.path) && Files.isRegularFile(Paths.get(download.path))) {
                lines.add(download.sha1 + "\t" + download.path);
            }
        }
        Files.write(INSTALLED, lines, StandardCharsets.UTF_8);
    }

    private static String sha1(Path path) throws IOException {
        MessageDigest digest = sha1Digest();
        try (InputStream in = Files.newInputStream(path)) {
            byte[] buffer = new byte[1 << 16];
            int read;
            while ((read = in.read(buffer)) > 0) {
                digest.update(buffer, 0, read);
            }
        }
        return hex(digest.digest());
    }

    private static MessageDigest sha1Digest() {
        try {
            return MessageDigest.getInstance("SHA-1");
        } catch (NoSuchAlgorithmException e) {
            throw new IllegalStateException(e);
        }
    }

    private static String hex(byte[] bytes) {
        char[] digits = "0123456789abcdef".toCharArray();
        char[] out = new char[bytes.length * 2];
        for (int i = 0; i < bytes.length; i++) {
            out[i * 2] = digits[(bytes[i] >> 4) & 0xf];
            out[i * 2 + 1] = digits[bytes[i] & 0xf];
        }
        return new String(out);
    }
}
