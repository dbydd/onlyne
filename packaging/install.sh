#!/usr/bin/env sh
# Install the Onlyne binaries from a GitHub Release.
#
#   sh packaging/install.sh                 # latest release
#   sh packaging/install.sh v1.3.1          # one tag
#   PREFIX=~/.local sh packaging/install.sh # another prefix
#
# The script picks the archive for this machine, checks it against the release's
# SHA256SUMS, and installs the five binaries into PREFIX/bin. It writes nothing
# outside that prefix and starts no service: the daemons run in the foreground,
# and the launchd and systemd units under packaging/ are how an operator runs
# them as services.
set -eu

repository=${ONLYNE_REPOSITORY:-dbydd/onlyne}
version=${1:-latest}
prefix=${PREFIX:-/usr/local}
base=https://github.com/$repository/releases

if [ "$version" = "latest" ]; then
  version=$(curl -fsSL https://api.github.com/repos/$repository/releases/latest \
    | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)
  if [ -z "$version" ]; then
    echo "install: no published release found for $repository" >&2
    exit 1
  fi
fi
bare=${version#v}

os=$(uname -s)
machine=$(uname -m)
case "$os" in
  Darwin) case "$machine" in
      arm64) target=aarch64-apple-darwin ;;
      x86_64) target=x86_64-apple-darwin ;;
      *) echo "install: unsupported mac architecture $machine" >&2; exit 1 ;;
    esac ;;
  Linux) case "$machine" in
      aarch64 | arm64) target=aarch64-unknown-linux-gnu ;;
      x86_64) target=x86_64-unknown-linux-gnu ;;
      *) echo "install: unsupported linux architecture $machine" >&2; exit 1 ;;
    esac ;;
  *) echo "install: unsupported operating system $os" >&2; exit 1 ;;
esac

archive=onlyne-$bare-$target.tar.gz
url=$base/download/$version/$archive
sums=$base/download/$version/SHA256SUMS

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

fetch() {
  if command -v curl >/dev/null 2>&1; then curl -fsSL "$1" -o "$2";
  elif command -v wget >/dev/null 2>&1; then wget -qO "$2" "$1";
  else echo "install: curl or wget is required" >&2; exit 1; fi
}

echo "install: $archive from $version"
fetch "$url" "$work/$archive"
fetch "$sums" "$work/SHA256SUMS"

# The checksum line is matched on the archive's own name, so a SHA256SUMS that
# carries other files does not confuse the check.
want=$(awk -v name="$archive" '$2 == name || $2 == "*"name { print $1 }' "$work/SHA256SUMS")
if [ -z "$want" ]; then
  echo "install: SHA256SUMS names no $archive; refusing to install an unverified file" >&2
  exit 1
fi
if command -v sha256sum >/dev/null 2>&1; then got=$(sha256sum "$work/$archive" | awk '{print $1}');
elif command -v shasum >/dev/null 2>&1; then got=$(shasum -a 256 "$work/$archive" | awk '{print $1}');
else echo "install: sha256sum or shasum is required" >&2; exit 1; fi
if [ "$got" != "$want" ]; then
  echo "install: checksum mismatch for $archive" >&2
  echo "  want $want" >&2
  echo "  got  $got" >&2
  exit 1
fi

tar -xzf "$work/$archive" -C "$work"
mkdir -p "$prefix/bin"
for bin in onlyne onlyne-server onlyne-client onlyne-gateway onlyne-tui; do
  cp "$work/onlyne-$bare-$target/$bin" "$prefix/bin/$bin"
  chmod 755 "$prefix/bin/$bin"
done
echo "install: five binaries in $prefix/bin"
