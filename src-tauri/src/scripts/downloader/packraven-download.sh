#!/usr/bin/env sh
# Downloads the mods this server pack lists in downloads.txt, next to this script, and checks each one's SHA-1.
# run.sh runs this before the server starts. Run it again any time; it only downloads what's missing.
# We keep it to POSIX sh with curl or wget, so it runs on minimal server images too.
# This script is in the pack's packraven folder, and the paths it downloads to start at the server's folder.
cd "$(dirname "$0")/.." || exit 1
manifest=packraven/downloads.txt
# We remember what we installed, so the next pack version can remove the files it dropped, and so a
# normal start doesn't hash every file again.
record=packraven/installed.txt
threads=8
attempts=3
tab=$(printf '\t')

if command -v sha1sum >/dev/null 2>&1; then
    sha1() { sha1sum "$1" | cut -d ' ' -f 1; }
elif command -v shasum >/dev/null 2>&1; then
    sha1() { shasum -a 1 "$1" | cut -d ' ' -f 1; }
else
    echo "This script needs sha1sum or shasum to check the downloads, and found neither." >&2
    exit 1
fi
if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL -A packraven-server-downloader -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -q -U packraven-server-downloader -O "$2" "$1"; }
else
    echo "This script needs curl or wget to download the pack's mods, and found neither." >&2
    exit 1
fi

# packraven 0.4.0 kept its files in the server's folder. We carry over what it installed and remove the rest,
# so unzipping a newer pack over an older one leaves that folder clean.
if [ -f .packraven-installed.txt ] && [ ! -f "$record" ]; then mv .packraven-installed.txt "$record"; fi
rm -f .packraven-installed.txt packraven-downloads.txt packraven-downloader.jar

safe_path() {
    case "$1" in '' | /* | *\\* | *:*) return 1 ;; esac
    case "/$1/" in */../* | */./* | *//*) return 1 ;; esac
}

recorded_sha1() {
    [ -f "$record" ] && awk -F '\t' -v path="$1" '$2 == path { print $1; exit }' "$record"
}

work=$(mktemp -d) || exit 1
trap 'rm -rf "$work"' EXIT
: > "$work/missing"
: > "$work/failed"

# We only remove a file the pack no longer lists if it's still the one we downloaded, so a jar you put
# at the same path yourself stays.
if [ -f "$record" ]; then
    while IFS="$tab" read -r sum path; do
        safe_path "$path" || continue
        if ! awk -F '\t' -v path="$path" '$4 == path { found = 1; exit } END { exit !found }' "$manifest" && [ -f "$path" ] && [ "$(sha1 "$path")" = "$sum" ]; then
            rm -f "$path"
            echo "Removed $path, which this version of the pack dropped"
        fi
    done < "$record"
fi

# A server admin who renames a jar to .disabled wants it off, so we don't download it again.
total=0
while IFS="$tab" read -r sum size url path; do
    case "$sum" in '' | '#'*) continue ;; esac
    if [ -z "$url" ] || ! safe_path "$path"; then
        echo "$manifest has a malformed line for $path" >&2
        exit 1
    fi
    total=$((total + 1))
    [ -e "$path.disabled" ] && continue
    if [ -f "$path" ] && [ "$(wc -c < "$path" | tr -d ' ')" = "$size" ]; then
        if [ "$(recorded_sha1 "$path")" = "$sum" ] || [ "$(sha1 "$path")" = "$sum" ]; then
            continue
        fi
    fi
    printf '%s\t%s\t%s\n' "$sum" "$url" "$path" >> "$work/missing"
done < "$manifest"

download() {
    sum=$1 url=$2 path=$3
    mkdir -p "$(dirname "$path")"
    attempt=1
    while [ "$attempt" -le "$attempts" ]; do
        if fetch "$url" "$path.part"; then
            actual=$(sha1 "$path.part")
            if [ "$actual" = "$sum" ]; then
                mv -f "$path.part" "$path"
                echo "Downloaded $path"
                return
            fi
            failure="$url sent a file with SHA-1 $actual instead of $sum"
        else
            failure="couldn't download $url"
        fi
        rm -f "$path.part"
        attempt=$((attempt + 1))
    done
    printf '%s\t%s\n' "$path" "$failure" >> "$work/failed"
}

count=$(wc -l < "$work/missing" | tr -d ' ')
if [ "$count" -gt 0 ]; then
    echo "Downloading $count of the pack's $total mod files"
    started=0
    while IFS="$tab" read -r sum url path; do
        download "$sum" "$url" "$path" < /dev/null &
        started=$((started + 1))
        # We wait for each group of downloads, which POSIX sh can do without job control.
        if [ $((started % threads)) -eq 0 ]; then wait; fi
    done < "$work/missing"
    wait
fi

# Every file that didn't fail is now verified: we checked its hash above, or we just downloaded it.
while IFS="$tab" read -r sum size url path; do
    case "$sum" in '' | '#'*) continue ;; esac
    [ -f "$path" ] || continue
    cut -f 1 "$work/failed" | grep -qxF -- "$path" && continue
    printf '%s\t%s\n' "$sum" "$path"
done < "$manifest" > "$work/installed"
cat "$work/installed" > "$record"

if [ -s "$work/failed" ]; then
    failed=$(wc -l < "$work/failed" | tr -d ' ')
    if [ "$failed" -eq 1 ]; then echo "Couldn't download 1 file:"; else echo "Couldn't download $failed files:"; fi
    while IFS="$tab" read -r path failure; do echo "  $path: $failure"; done < "$work/failed"
    echo "Check your internet connection, then start the server again. We only download the files that are still missing."
    exit 1
fi
