#!/usr/bin/env sh
cd "$(dirname "$0")" || exit 1

# To run the server on your own Java, write the path to its java executable in java.txt.
# Add JVM arguments, like -Xmx for memory, to user_jvm_args.txt.
GRAALVM="{{GRAALVM}}"
JAVA=""
if [ -f java.txt ]; then
    JAVA=$(head -n 1 java.txt | tr -d '\r"')
fi

if [ -z "$JAVA" ] && [ -z "$GRAALVM" ]; then
    JAVA=java
elif [ -z "$JAVA" ]; then
    GRAALVM_DIR="java/graalvm-$GRAALVM"
    case "$(uname -s)" in
        Darwin) OS=macos; JAVA="$GRAALVM_DIR/Contents/Home/bin/java" ;;
        *) OS=linux; JAVA="$GRAALVM_DIR/bin/java" ;;
    esac
    case "$(uname -m)" in
        x86_64 | amd64) ARCH=x64 ;;
        aarch64 | arm64) ARCH=aarch64 ;;
        *) echo "Oracle has no GraalVM build for $(uname -m). Install Java $GRAALVM yourself, then write the path to its java executable in java.txt."; exit 1 ;;
    esac
    if [ ! -x "$JAVA" ]; then
        echo "Downloading GraalVM $GRAALVM into $GRAALVM_DIR. To use your own Java instead, write its path in java.txt."
        URL="{{URL_PREFIX}}$OS-${ARCH}_bin.tar.gz"
        rm -rf "$GRAALVM_DIR" && mkdir -p "$GRAALVM_DIR"
        if command -v curl > /dev/null 2>&1; then
            curl -fL "$URL" | tar -xz -C "$GRAALVM_DIR" --strip-components=1
        elif command -v wget > /dev/null 2>&1; then
            wget -qO- "$URL" | tar -xz -C "$GRAALVM_DIR" --strip-components=1
        else
            echo "Downloading GraalVM needs curl or wget, and this system has neither."
        fi
        if [ ! -x "$JAVA" ]; then
            rm -rf "$GRAALVM_DIR"
            echo "Couldn't download GraalVM $GRAALVM. Install Java $GRAALVM yourself, then write the path to its java executable in java.txt."
            exit 1
        fi
    fi
fi

{{DOWNLOAD}}
{{INSTALL}}
echo "Starting the server on $JAVA"
exec "$JAVA" @user_jvm_args.txt {{LAUNCH}} nogui{{ARGS}} "$@"
