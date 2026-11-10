// packraven writes this file into every server pack as PLEASE READ - IMPORTANT!.txt, leaving out lines that start with //.
// Placeholders packraven fills in:
//   {name}                the pack's name
//   {version}             the version you're building
//   {minecraft}           the Minecraft version
//   {loader}              NeoForge, Forge or Fabric
//   {loader_version}      the loader's version, like 21.1.265
//   {setup}               what the run script does on first start, like "downloads our mods and installs NeoForge and Minecraft"
//   {where_are_the_mods}  a note that the run script downloads the mods, or nothing when the server pack includes them
//   {jar_only_hosts}      the steps for a host that only lets you pick a server jar or loader version
//   {packraven_folder}    a note on the packraven folder, or nothing when the server pack has none
// When a placeholder alone on its line fills in as nothing, packraven leaves out that line and the blank line after it.

{name} {version} server

{where_are_the_mods}

On its first start, the run script installs Java, then {setup}.


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
1. Upload everything inside this folder (every file and folder in it, but not this folder itself)
   to your server's main folder, using your host's file manager or SFTP.
2. Set the startup command to: sh run.sh
3. Start the server. The first time, the run script {setup}.

IF YOUR HOST CAN'T RUN run.sh
{jar_only_hosts}


SETTINGS
- Memory (RAM): change the -Xmx line in user_jvm_args.txt, like -Xmx8G for 8 GB. Other Java flags go there too.
- To use your own Java, write the full path to its java executable in a new file named java.txt in this folder.

{packraven_folder}
