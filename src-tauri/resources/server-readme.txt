// packraven writes this file into every server pack as PLEASE READ - IMPORTANT!.txt, leaving out lines that start with //.
// Placeholders packraven fills in:
//   {name}              the pack's name
//   {version}           the version you're building
//   {minecraft}         the Minecraft version
//   {loader}            NeoForge, Forge or Fabric
//   {loader_version}    the loader's version, like 21.1.265
//   {setup}             what the first start does, like "downloads our mods and installs NeoForge and Minecraft"
//   {packraven_folder}  a note on the packraven folder, or nothing when the server pack has none

{name} {version} server

WHERE ARE THE MODS?
They get downloaded automatically when you start the server!

The run script installs Java, {setup}.

HOW TO START
Windows: double-click run.bat.
Linux or macOS: open a terminal, go to this folder (cd /path/to/this/folder), then run:
    chmod +x run.sh
    ./run.sh
chmod +x marks the script as runnable, so you only need it the first time. Or skip it and run: sh run.sh

You'll need to accept the Minecraft EULA. Depending on version, the terminal will prompt you or stop and ask you to edit the eula.txt file.

The first start can take a few minutes. Later starts are much faster.
To stop the server safely, type stop in its window and press Enter.

FOR SERVER HOSTS
1. Upload everything inside this folder (not the folder itself) to your server's main folder,
   using your host's file manager or SFTP.
2. Set the startup command to: sh run.sh
3. Start!

If your host only lets you pick a server jar or loader version, ask their support to start the
server with our run.sh instead, or to support packraven server packs: https://packraven.app/
If they can't, start the server on your own computer once so the mods download, stop it, then upload
everything in this folder and select {loader} {loader_version} for Minecraft {minecraft}.

SETTINGS
- Memory (RAM): change the -Xmx line in user_jvm_args.txt, like -Xmx8G for 8 GB. Other Java flags go there too.
- To use your own Java, write the full path to its java executable in a new file named java.txt in this folder.

{packraven_folder}
