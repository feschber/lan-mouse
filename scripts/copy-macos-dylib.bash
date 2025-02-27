#!/usr/bin/env bash
set -e

homebrew_path=""
exec_path="target/debug/bundle/osx/Lan Mouse.app/Contents/MacOS/lan-mouse"

usage() {
    cat <<EOF
$0: Copy all Homebrew libraries into the macOS app bundle.
USAGE: $0 [-h] [-b homebrew_path] [exec_path]

OPTIONS:
  -h, --help    Show this help message and exit
  -b            Path to Homebrew installation (default: $homebrew_path)
  exec_pat      Path to the main executable in the app bundle
                (default: get from `brew --prefix`)

When macOS apps are linked to dynamic libraries (.dylib files),
the fully qualified path to the library is embedded in the binary.
If the libraries come from Homebrew, that means that Homebrew must be present
and the libraries must be installed in the same location on the user's machine.

This script copies all of the Homebrew libraries that an executable links to into the app bundle
and tells all the binaries in the bundle to look for them there.
EOF
}

# Gather command-line arguments
while test $# -gt 0; do
    case "$1" in
        -h | --help ) usage; exit 0;;
        -b | --homebrew ) homebrew_path="$1"; shift 2;;
        * ) exec_path="$1"; shift;;
    esac
done

if [ -z "$homebrew_path" ]; then
    homebrew_path="$(brew --prefix)"
fi

# Path to the .app bundle
bundle_path=$(dirname "$(dirname "$(dirname "$exec_path")")")
# Path to the Frameworks directory
fwks_path="$bundle_path/Contents/Frameworks"
mkdir -p "$fwks_path"
# Array of all the binaries we need to process, starting with the main executable.
queue=("$exec_path")
# Array of all the libraries we've already copied, so we don't do it twice.
declare -A copied

# Copy and fix references for a binary (executable or dylib)
#
# This function will:
# - Copy any referenced dylibs from /opt/homebrew to the Frameworks directory
# - Update the binary to reference the local copy instead
# - Add the Frameworks directory to the binary's RPATH
# - Add the binary to the queue to process its own references
fix_references() {
  local bin="$1"

  # Make an array of all Homebrew libraries referenced by the binary
  local libs
  mapfile -t libs < <(otool -L "$bin" | awk -v homebrew="$homebrew_path" '$0 ~ homebrew {print $1}')

  for old_path in "${libs[@]}"; do
    local base_name="$(basename "$old_path")"
    local dest="$fwks_path/$base_name"

    if [[ -z "${copied["$old_path"]}" ]]; then
      echo "Copying $old_path -> $dest"
      cp -f "$old_path" "$dest"
      copied["$old_path"]=1

      echo "Updating $dest to have install_name of @rpath/$base_name..."
      install_name_tool -id "@rpath/$base_name" "$dest"

      # Add this dylib to the queue to process its own references
      queue+=("$dest")
    fi

    echo "Updating $bin to reference @rpath/$base_name..."
    install_name_tool -change "$old_path" "@rpath/$base_name" "$bin"
  done
}

# Run fix_references on each binary in the queue.
# We keep adding new binaries to the queue as we find them, so this will process all of them.
while [[ ${#queue[@]} -gt 0 ]]; do
  current="${queue[0]}"
  queue=("${queue[@]:1}")  # pop front

  fix_references "$current"
done

# Ensure the main executable has our Frameworks path in its RPATH
if ! otool -l "$exec_path" | grep -q "@executable_path/../Frameworks"; then
  echo "Adding RPATH to $exec_path"
  install_name_tool -add_rpath "@executable_path/../Frameworks" "$exec_path"
fi

# Se-sign the .app
codesign --force --deep --sign - "$bundle_path"

echo "Done!"
