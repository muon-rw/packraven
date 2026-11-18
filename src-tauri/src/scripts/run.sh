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
    URL="{{URL_PREFIX}}$OS-${ARCH}_bin.tar.gz"
    if command -v curl > /dev/null 2>&1; then
        CHECK="curl -fsL --max-time 20"; FETCH="curl -fL"
    elif command -v wget > /dev/null 2>&1; then
        CHECK="wget -qO- -T 20"; FETCH="wget -qO-"
    else
        CHECK=""; FETCH=""
    fi

    # Oracle replaces its latest GraalVM build in place, so we compare the checksum Oracle publishes
    # with the one we saved when we installed. Offline, we keep using what's installed.
    LATEST=""
    if [ -n "$CHECK" ]; then
        LATEST=$($CHECK "$URL.sha256" 2> /dev/null | cut -d ' ' -f 1)
    fi
    INSTALLED=""
    if [ -f "$GRAALVM_DIR/download.sha256" ]; then
        INSTALLED=$(cat "$GRAALVM_DIR/download.sha256")
    fi

    if [ ! -x "$JAVA" ] || { [ -n "$LATEST" ] && [ "$LATEST" != "$INSTALLED" ]; }; then
        if [ -x "$JAVA" ]; then
            echo "Updating GraalVM $GRAALVM in $GRAALVM_DIR."
        else
            echo "Downloading GraalVM $GRAALVM into $GRAALVM_DIR. To use your own Java instead, write its path in java.txt."
        fi
        STAGING="$GRAALVM_DIR.new"
        rm -rf "$STAGING" && mkdir -p "$STAGING"
        if [ -n "$FETCH" ]; then
            $FETCH "$URL" | tar -xz -C "$STAGING" --strip-components=1
        else
            echo "Downloading GraalVM needs curl or wget, and this system has neither."
        fi
        if [ -x "$STAGING${JAVA#"$GRAALVM_DIR"}" ]; then
            if [ -n "$LATEST" ]; then
                echo "$LATEST" > "$STAGING/download.sha256"
            fi
            rm -rf "$GRAALVM_DIR" && mv "$STAGING" "$GRAALVM_DIR"
        else
            rm -rf "$STAGING"
            if [ ! -x "$JAVA" ]; then
                echo "Couldn't download GraalVM $GRAALVM. Install Java $GRAALVM yourself, then write the path to its java executable in java.txt."
                exit 1
            fi
            echo "Couldn't update GraalVM $GRAALVM, so the server will start on the version it already has."
        fi
    fi
fi

{{DOWNLOAD}}
{{INSTALL}}
echo "Starting the server on $JAVA"
exec "$JAVA" @user_jvm_args.txt {{LAUNCH}} nogui{{ARGS}} "$@"
