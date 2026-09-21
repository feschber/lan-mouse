#!/bin/bash
set -eu

homebrew_path=""
default_exec_path="target/debug/bundle/osx/Lan Mouse.app/Contents/MacOS/lan-mouse"
exec_path="$default_exec_path"

usage() {
    cat <<EOF
$0: Copy all Homebrew libraries into the macOS app bundle.
USAGE: $0 [-h] [-b homebrew_path] [exec_path]

OPTIONS:
  -h, --help    Show this help message and exit
  -b            Path to Homebrew installation
                (default: obtained from 'brew --prefix')
  exec_path     Path to the main executable in the app bundle
                (default: $default_exec_path)

When macOS apps are linked to dynamic libraries (.dylib files),
the fully qualified path to the library is embedded in the binary.
If the libraries come from Homebrew, that means that Homebrew must be present
and the libraries must be installed in the same location on the user's machine.

This script copies all of the Homebrew libraries that an executable links to into the app bundle
and tells all the binaries in the bundle to look for them there.
EOF
}

exec_path_set=0

# Accept the single positional argument, rejecting a second one rather than
# silently letting it win.
set_exec_path() {
    if [ "$exec_path_set" = 1 ]; then
        echo "$0: unexpected extra argument: $1" >&2
        usage >&2
        exit 1
    fi
    exec_path="$1"
    exec_path_set=1
}

# Gather command-line arguments
while test $# -gt 0; do
    case "$1" in
        -h | --help ) usage; exit 0;;
        -b | --homebrew )
            if [ $# -lt 2 ]; then
                echo "$0: $1 requires an argument" >&2
                exit 1
            fi
            homebrew_path="$2"; shift 2;;
        -- )
            # Everything after this is positional, so an exec_path may start
            # with a dash.
            shift
            while test $# -gt 0; do
                set_exec_path "$1"; shift
            done;;
        -* ) echo "$0: unknown option: $1" >&2; usage >&2; exit 1;;
        * ) set_exec_path "$1"; shift;;
    esac
done

# `dirname` and friends would read a leading dash as an option of their own.
case "$exec_path" in
    -* ) exec_path="./$exec_path";;
esac

if [ -z "$homebrew_path" ]; then
    homebrew_path="$(brew --prefix)"
fi
# Normalise away a trailing slash so the prefix tests below can rely on the
# separator being present exactly once.
homebrew_path="${homebrew_path%/}"

# Path to the .app bundle
bundle_path=$(dirname "$(dirname "$(dirname "$exec_path")")")
# Path to the Frameworks directory
fwks_path="$bundle_path/Contents/Frameworks"
mkdir -p "$fwks_path"
# Path to bundled GTK/GSettings data
resources_path="$bundle_path/Contents/Resources"
share_path="$resources_path/share"
# Directory holding the main executable, used to expand @executable_path
exec_dir="$(cd "$(dirname "$exec_path")" && pwd)"
# Accumulates references that could not be found on this machine. The loops
# below run in the current shell (via here-strings rather than pipes), so a
# plain global works even across the recursion.
unresolved=""

# Records which library each bundled file name came from, as
# "<base_name> <device>:<inode> <source_path>" lines, to detect collisions.
bundled_from=""

# Identify a file by device and inode, following symlinks, so that the same
# library reached through different paths -- $homebrew_path/lib/libfoo.dylib and
# $homebrew_path/opt/foo/lib/libfoo.dylib are both symlinks into the Cellar --
# is recognised as one library rather than as a collision.
file_id() {
  stat -L -f '%d:%i' "$1"
}

# Print the LC_RPATH entries of a binary, one per line, deduplicated: a
# universal binary lists its load commands once per architecture.
binary_rpaths() {
  otool -l "$1" | awk '
    /^ *cmd LC_RPATH$/ { in_rpath = 1; next }
    in_rpath && /^ *path / {
      sub(/^ *path /, "")
      sub(/ \(offset [0-9]+\)$/, "")
      print
      in_rpath = 0
    }' | sort -u
}

# Resolve a @rpath/@loader_path/@executable_path reference to a real file on
# this machine, echoing its path. Returns non-zero if nothing matches.
#
# Usage: resolve_ref <reference> <origin_dir> <rpath_source>
#
# <origin_dir> is the directory the referencing binary *originally* lived in,
# not its copy under Frameworks: @loader_path is relative to that, both in a
# load command and inside an LC_RPATH entry. <rpath_source> is the binary whose
# LC_RPATH entries are searched, i.e. the copy, whose entries are still intact
# at the point this is called.
resolve_ref() {
  local ref="$1" origin_dir="$2" rpath_source="$3"
  local rpath candidate

  case "$ref" in
    @loader_path/* )
      candidate="$origin_dir/${ref#@loader_path/}"
      if [ -e "$candidate" ]; then
        echo "$candidate"
        return 0
      fi;;
    @executable_path/* )
      candidate="$exec_dir/${ref#@executable_path/}"
      if [ -e "$candidate" ]; then
        echo "$candidate"
        return 0
      fi;;
    @rpath/* )
      # dyld tries each LC_RPATH entry of the loading binary in order.
      while IFS= read -r rpath; do
        if [ -z "$rpath" ]; then
          continue
        fi
        case "$rpath" in
          @loader_path* ) rpath="$origin_dir${rpath#@loader_path}";;
          @executable_path* ) rpath="$exec_dir${rpath#@executable_path}";;
        esac
        candidate="$rpath/${ref#@rpath/}"
        if [ -e "$candidate" ]; then
          echo "$candidate"
          return 0
        fi
      done <<< "$(binary_rpaths "$rpath_source")"
      ;;
  esac

  # Fall back to the Homebrew library directory, which covers the common case
  # of a formula that symlinks its dylibs there.
  candidate="$homebrew_path/lib/$(basename "$ref")"
  if [ -e "$candidate" ]; then
    echo "$candidate"
    return 0
  fi

  return 1
}

# Remove LC_RPATH entries pointing into this machine's Homebrew tree.
#
# dyld searches the loading binary's own RPATHs before those of the binaries
# above it in the load chain, so a leftover Cellar path would win over the
# bundled copy on a user's machine that happens to have the same formula
# installed -- exactly what bundling is meant to prevent.
strip_homebrew_rpaths() {
  local bin="$1" rpath

  while IFS= read -r rpath; do
    if [ -z "$rpath" ]; then
      continue
    fi
    case "$rpath" in
      "$homebrew_path"/* )
        echo "Removing Homebrew RPATH $rpath from $bin"
        install_name_tool -delete_rpath "$rpath" "$bin";;
    esac
  done <<< "$(binary_rpaths "$bin")"
}

# Copy a library into the Frameworks directory unless it is already there, give
# it an install name of @rpath/<base_name> and recursively process its own
# dependencies.
#
# Usage: bundle_lib <base_name> <source_path>
# Returns non-zero (after warning) if <source_path> does not exist.
bundle_lib() {
  local base_name="$1"
  local source_path="$2"
  local dest="$fwks_path/$base_name"
  local source_id previous

  # A reference the bundle already satisfies resolves to the copy itself. This
  # covers a dylib's own install name, which shows up in its `otool -L` output.
  case "$source_path" in
    "$fwks_path"/* ) return 0;;
  esac

  if [ ! -e "$source_path" ]; then
    echo "Warning: Could not find $base_name at $source_path" >&2
    return 1
  fi

  source_id="$(file_id "$source_path")"
  previous="$(printf '%s' "$bundled_from" \
    | awk -v name="$base_name" '$1 == name { print $2; exit }')"

  if [ -n "$previous" ]; then
    # Frameworks is flat, so a file name identifies a library. Two different
    # libraries sharing one name cannot both be bundled, and the second
    # binary's reference would be pointed at the first library instead -- a
    # mismatch that only shows up as a missing symbol at runtime. Bundling
    # these would mean giving them distinct names or nesting them, so refuse
    # rather than produce a bundle that is quietly wrong.
    if [ "$previous" != "$source_id" ]; then
      echo >&2
      echo "$0: two different libraries are both named $base_name:" >&2
      printf '%s' "$bundled_from" \
        | awk -v name="$base_name" '$1 == name { print "  " $3 }' >&2
      echo "  $source_path" >&2
      exit 1
    fi

    # Same library, already bundled.
    return 0
  fi

  bundled_from="$bundled_from$base_name $source_id $source_path
"

  # Left over from an earlier run: its references were fixed then, so there is
  # nothing to copy or walk.
  if [ -e "$dest" ]; then
    return 0
  fi

  echo "Copying $source_path -> $dest"
  cp -f "$source_path" "$dest"
  # Ensure the copied dylib is writable so that xattr -rd /path/to/Lan\ Mouse.app works.
  chmod 644 "$dest"

  echo "Updating $dest to have install_name of @rpath/$base_name..."
  install_name_tool -id "@rpath/$base_name" "$dest"

  # Recursively process this dylib. Pass the directory it came from so that
  # @loader_path references can still be resolved against it.
  fix_references "$dest" "$(dirname "$source_path")"

  # Only now that its references have been resolved is it safe to drop the
  # build machine's Homebrew paths.
  strip_homebrew_rpaths "$dest"

  # Let the bundled libraries find each other directly, so they resolve even
  # when loaded outside the main executable's load chain (e.g. via dlopen).
  if ! binary_rpaths "$dest" | grep -qxF '@loader_path/.'; then
    install_name_tool -add_rpath '@loader_path/.' "$dest"
  fi

  return 0
}

# Copy and fix references for a binary (executable or dylib)
#
# This function will:
# - Copy any referenced dylibs from the Homebrew prefix to the Frameworks directory
# - Update the binary to reference the local copy instead
# - Recursively process the copied dylibs
#
# Usage: fix_references <binary> [<origin_dir>]
#
# <origin_dir> defaults to the binary's own directory and is only meaningful
# for a copy under Frameworks, where it names the directory the library came
# from (see resolve_ref).
#
# The Frameworks directory is added to the RPATH of the main executable (see
# the end of this script) and each bundled dylib gets @loader_path/., so an
# @rpath reference resolves from anywhere in the bundle.
fix_references() {
  local bin="$1"
  local origin_dir="${2:-$(dirname "$bin")}"
  local libs relative_libs old_path ref base_name source_path

  # Get all Homebrew libraries referenced by the binary (absolute paths)
  # The trailing slash keeps a prefix of /opt/homebrew from also matching a
  # path like /opt/homebrew-testing/lib/....
  libs=$(otool -L "$bin" | awk -v homebrew="$homebrew_path/" 'index($1, homebrew) == 1 {print $1}')

  # References made through one of dyld's placeholders. These need resolving
  # against the referencing binary before they can be copied.
  relative_libs=$(otool -L "$bin" | awk '$1 ~ /^@(rpath|loader_path|executable_path)\// {print $1}')

  while IFS= read -r old_path; do
    if [ -z "$old_path" ]; then
      continue
    fi

    base_name="$(basename "$old_path")"

    # On failure, leave the absolute reference alone: rewriting it to @rpath
    # when nothing was copied to Frameworks would break the binary outright.
    if ! bundle_lib "$base_name" "$old_path"; then
      unresolved="$unresolved  $old_path (referenced by $bin)
"
      continue
    fi

    echo "Updating $bin to reference @rpath/$base_name..."
    install_name_tool -change "$old_path" "@rpath/$base_name" "$bin"
  done <<< "$libs"

  while IFS= read -r ref; do
    if [ -z "$ref" ]; then
      continue
    fi

    base_name="$(basename "$ref")"

    if [ -e "$fwks_path/$base_name" ]; then
      # Already bundled. This also covers a dylib's own install name, which
      # shows up in its own `otool -L` output.
      source_path="$fwks_path/$base_name"
    elif ! source_path="$(resolve_ref "$ref" "$origin_dir" "$bin")"; then
      echo "Warning: Could not resolve $ref referenced by $bin" >&2
      unresolved="$unresolved  $ref (referenced by $bin)
"
      continue
    fi

    if ! bundle_lib "$base_name" "$source_path"; then
      unresolved="$unresolved  $ref (referenced by $bin)
"
      continue
    fi

    # An @rpath reference can stay as it is -- it resolves to the Frameworks
    # directory through the RPATHs in the load chain. @loader_path and
    # @executable_path are relative to the loading binary and to the
    # executable, which is wrong once the library has been moved into
    # Frameworks, so rewrite those to @rpath.
    case "$ref" in
      @rpath/* ) ;;
      * )
        echo "Updating $bin to reference @rpath/$base_name..."
        install_name_tool -change "$ref" "@rpath/$base_name" "$bin";;
    esac
  done <<< "$relative_libs"
}

fix_references "$exec_path"
strip_homebrew_rpaths "$exec_path"

# A dependency that could not be found is not a warning to scroll past: the
# bundle would build successfully and then fail to launch on any machine
# without Homebrew, which is the whole point of this script.
if [ -n "$unresolved" ]; then
  echo >&2
  echo "$0: the following dependencies are missing from the bundle:" >&2
  printf '%s' "$unresolved" >&2
  echo "The bundle would fail to launch on a machine without Homebrew." >&2
  exit 1
fi

copy_runtime_data() {
  mkdir -p "$share_path"

  if [ -d "$homebrew_path/share/glib-2.0/schemas" ]; then
    mkdir -p "$share_path/glib-2.0"
    rm -rf "$share_path/glib-2.0/schemas"
    cp -RL "$homebrew_path/share/glib-2.0/schemas" "$share_path/glib-2.0/schemas"
    if command -v glib-compile-schemas >/dev/null 2>&1; then
      glib-compile-schemas "$share_path/glib-2.0/schemas"
    elif [ -x "$homebrew_path/bin/glib-compile-schemas" ]; then
      "$homebrew_path/bin/glib-compile-schemas" "$share_path/glib-2.0/schemas"
    fi
  fi

  if [ -d "$homebrew_path/share/gtk-4.0" ]; then
    rm -rf "$share_path/gtk-4.0"
    cp -RL "$homebrew_path/share/gtk-4.0" "$share_path/gtk-4.0"
  fi

  if [ -d "$homebrew_path/share/icons/Adwaita" ]; then
    mkdir -p "$share_path/icons"
    rm -rf "$share_path/icons/Adwaita"
    cp -RL "$homebrew_path/share/icons/Adwaita" "$share_path/icons/Adwaita"
  fi
}

copy_runtime_data

# cargo-bundle preserves the source path under Contents/Resources (so
# `target/menubar-template.png` lands at `Resources/target/...`). Flatten it
# so NSBundle pathForResource: finds the file at the Resources root.
if [ -f "$resources_path/target/menubar-template.png" ]; then
  mv "$resources_path/target/menubar-template.png" "$resources_path/menubar-template.png"
  rmdir "$resources_path/target" 2>/dev/null || true
fi

# Ensure the main executable has our Frameworks path in its RPATH
if ! otool -l "$exec_path" | grep -qF "@executable_path/../Frameworks"; then
  echo "Adding RPATH to $exec_path"
  install_name_tool -add_rpath "@executable_path/../Frameworks" "$exec_path"
fi

# Sign the .app. Nested code has to be signed inside-out: sign the bundled
# libraries first, then the bundle itself (`codesign --deep` is deprecated).
# Everything under Frameworks was copied there by bundle_lib, so every file is
# Mach-O nested code and needs a signature -- matching on *.dylib would leave a
# differently named library (e.g. a .so) unsigned and the bundle seal invalid.
find "$fwks_path" -type f -exec codesign --force --sign - {} +
codesign --force --sign - "$bundle_path"

echo "Done!"
