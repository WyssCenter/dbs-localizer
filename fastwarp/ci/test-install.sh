#!/bin/sh
# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0
#
# End-to-end test of scripts/install.sh against a fake release folder: install from file:// and
# http://, checksum refusal, PATH lines in the shell profiles, uninstall. Needs cc, curl and python3.
# shellcheck disable=SC2016

set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
installer=$root/scripts/install.sh
work=$(mktemp -d)
server_pid=
cleanup() {
    [ -z "$server_pid" ] || kill "$server_pid" 2>/dev/null || true
    rm -rf "$work"
}
trap cleanup EXIT

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

pass() {
    printf 'ok: %s\n' "$*"
}

case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) target=x86_64-unknown-linux-gnu ;;
    Linux-aarch64) target=aarch64-unknown-linux-gnu ;;
    Darwin-arm64) target=aarch64-apple-darwin ;;
    Darwin-x86_64) target=x86_64-apple-darwin ;;
    *) fail "no release target for $(uname -s) $(uname -m)" ;;
esac
for tool in cc curl python3; do
    command -v "$tool" >/dev/null || fail "$tool is needed for this test"
done

sha256() {
    if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | awk '{print $1}'
}

pkg=$work/pkg/fastwarp-$target
mkdir -p "$pkg" "$work/good" "$work/bad"
printf '#include <stdio.h>\nint main(void) { puts("fastwarp 0.0.0-test"); return 0; }\n' >"$work/dummy.c"
cc -o "$pkg/fastwarp" "$work/dummy.c"
cp "$root/LICENSE" "$root/NOTICE" "$pkg/"
archive=fastwarp-$target.tar.gz
tar -C "$work/pkg" -czf "$work/good/$archive" "fastwarp-$target"
printf '%s  %s\n' "$(sha256 "$work/good/$archive")" "$archive" >"$work/good/$archive.sha256"
cp "$work/good/$archive" "$work/bad/"
printf '%064d  %s\n' 0 "$archive" >"$work/bad/$archive.sha256"

home=$work/home
mkdir -p "$home"
base_path=/usr/bin:/bin:/usr/sbin:/sbin
dl=file://$work/good
test_path=$base_path
test_shell=/bin/bash
test_env=FASTWARP_TEST_UNUSED=1
script=$installer

run() {
    env -i HOME="$home" PATH="$test_path" SHELL="$test_shell" TMPDIR="$work" \
        FASTWARP_DOWNLOAD_URL="$dl" "$test_env" sh "$script" "$@"
}

markers() {
    if [ -f "$1" ]; then grep -c 'fastwarp-installer$' "$1" || true; else echo 0; fi
}

bin=$home/.local/bin/fastwarp
printf 'alias ll="ls -l"\n' >"$home/.bashrc"

dl=file://$work/bad
if out=$(run --version 0.0.0 2>&1); then fail "a wrong checksum was accepted: $out"; fi
printf '%s' "$out" | grep -q 'checksum mismatch' || fail "no checksum message: $out"
[ ! -e "$bin" ] || fail "the binary was installed despite the wrong checksum"
[ "$(markers "$home/.profile")" = 0 ] || fail "a profile was changed despite the wrong checksum"
pass "wrong checksum refused, nothing installed"

dl=file://$work/good
if out=$(run 2>&1); then fail "the repository copy installed without a version: $out"; fi
printf '%s' "$out" | grep -q 'not from a release' || fail "no missing-version message: $out"
pass "unstamped installer needs a version"

run --version 0.0.0 >"$work/log" 2>&1 || fail "install failed: $(cat "$work/log")"
[ -x "$bin" ] || fail "$bin missing"
[ "$("$bin")" = "fastwarp 0.0.0-test" ] || fail "$bin is not the release binary"
[ "$(markers "$home/.profile")" = 1 ] || fail ".profile has no PATH line"
[ "$(markers "$home/.bashrc")" = 1 ] || fail ".bashrc has no PATH line"
grep -q 'alias ll' "$home/.bashrc" || fail ".bashrc lost its content"
grep -q "$home/.profile" "$work/log" || fail "the changed files were not reported"
found=$(env -i HOME="$home" PATH="$base_path" sh -c '. "$HOME/.profile"; . "$HOME/.profile"; command -v fastwarp; printf "%s\n" "$PATH"')
[ "$(printf '%s\n' "$found" | head -n 1)" = "$bin" ] || fail "sourcing .profile does not put fastwarp on PATH: $found"
[ "$(printf '%s\n' "$found" | tail -n 1 | tr ':' '\n' | grep -cx "$home/.local/bin")" = 1 ] || fail "sourcing .profile twice repeats the folder in PATH"
pass "install from file:// with PATH lines in .profile and .bashrc"

run --version 0.0.0 >/dev/null 2>&1 || fail "reinstall failed"
[ "$(markers "$home/.profile")" = 1 ] && [ "$(markers "$home/.bashrc")" = 1 ] || fail "reinstall duplicated a PATH line"
pass "reinstall is idempotent"

run --uninstall >"$work/log" 2>&1 || fail "uninstall failed: $(cat "$work/log")"
[ ! -e "$bin" ] || fail "uninstall left $bin"
[ ! -d "$home/.local/bin" ] || fail "uninstall left the empty folder"
[ "$(markers "$home/.profile")" = 0 ] && [ "$(markers "$home/.bashrc")" = 0 ] || fail "uninstall left a PATH line"
grep -q 'alias ll' "$home/.bashrc" || fail "uninstall damaged .bashrc"
pass "uninstall removes the binary, the folder and the PATH lines"

python3 -m http.server --bind 127.0.0.1 --directory "$work/good" 18765 >"$work/http.log" 2>&1 &
server_pid=$!
i=0
until curl -fsS -o /dev/null "http://127.0.0.1:18765/$archive.sha256" 2>/dev/null; do
    i=$((i + 1))
    [ "$i" -lt 50 ] || fail "the HTTP server did not start: $(cat "$work/http.log")"
    sleep 0.2
done
custom="$work/custom dir/bin"
dl=http://127.0.0.1:18765
run --version v0.0.0 --dir "$custom" --no-modify-path >"$work/log" 2>&1 || fail "install over HTTP failed: $(cat "$work/log")"
[ -x "$custom/fastwarp" ] || fail "--dir was not used"
[ "$(markers "$home/.profile")" = 0 ] || fail "--no-modify-path changed .profile"
grep -q 'export PATH=' "$work/log" || fail "--no-modify-path printed no PATH instructions"
pass "install over http:// into --dir with --no-modify-path"

touch "$custom/other-program"
test_env=FASTWARP_INSTALL_DIR=$custom
run --version 0.0.0 >/dev/null 2>&1 || fail "install into FASTWARP_INSTALL_DIR failed"
[ "$(markers "$home/.profile")" = 1 ] || fail "no PATH line for FASTWARP_INSTALL_DIR"
found=$(env -i HOME="$home" PATH="$base_path" sh -c '. "$HOME/.profile"; command -v fastwarp')
[ "$found" = "$custom/fastwarp" ] || fail "the PATH line for a folder with a space does not work: $found"
grep -q '^sha256 ' "$custom/.fastwarp-installer" || fail "no receipt next to the program"
grep -qx "posix:$custom:$home/.profile" "$custom/.fastwarp-installer" || fail "the receipt does not list .profile"
run --uninstall >"$work/log" 2>&1 || fail "uninstall from FASTWARP_INSTALL_DIR failed: $(cat "$work/log")"
[ ! -e "$custom/fastwarp" ] || fail "uninstall left $custom/fastwarp"
[ ! -e "$custom/.fastwarp-installer" ] || fail "uninstall left the receipt"
[ -e "$custom/other-program" ] || fail "uninstall removed another program"
[ "$(markers "$home/.profile")" = 0 ] || fail "uninstall kept the PATH line of a folder that holds other programs"
grep -q 'still holds other files' "$work/log" || fail "no note about the other files"
rm "$custom/other-program"
pass "uninstall removes the PATH lines it added, also when the folder holds other programs"

test_env=FASTWARP_NO_MODIFY_PATH=1
run --version 0.0.0 --dir "$custom" >/dev/null 2>&1 || fail "install with FASTWARP_NO_MODIFY_PATH failed"
[ "$(markers "$home/.profile")" = 0 ] || fail "FASTWARP_NO_MODIFY_PATH=1 changed .profile"
run --uninstall --dir "$custom" >/dev/null 2>&1 || fail "uninstall failed"
test_env=FASTWARP_TEST_UNUSED=1
pass "FASTWARP_NO_MODIFY_PATH=1"

dl=file://$work/good
test_path=$home/.local/bin:$base_path
run --version 0.0.0 >"$work/log" 2>&1 || fail "install with the folder on PATH failed"
test_path=$base_path
[ "$(markers "$home/.profile")" = 0 ] || fail "a folder already on PATH was added again"
grep -q 'already on PATH' "$work/log" || fail "no note that the folder is on PATH"
run --uninstall >/dev/null 2>&1 || fail "uninstall failed"
pass "folder already on PATH: profiles unchanged"

mkdir -p "$home/.local/bin"
ln -s /bin/true "$bin"
if run --version 0.0.0 >"$work/log" 2>&1; then fail "a symbolic link (uv tool) was overwritten"; fi
grep -q 'not installed by this installer' "$work/log" || fail "no message about the foreign file"
if run --uninstall >/dev/null 2>&1; then fail "uninstall removed a symbolic link"; fi
[ -L "$bin" ] || fail "the symbolic link was removed"
rm "$bin"
printf '#!/bin/sh\necho pip\n' >"$bin"
if run --version 0.0.0 >/dev/null 2>&1; then fail "a script (pip entry point) was overwritten"; fi
rm "$bin"
cp "$pkg/fastwarp" "$bin"
if run --version 0.0.0 >/dev/null 2>&1; then fail "a program without a receipt (built by hand) was overwritten"; fi
if run --uninstall >/dev/null 2>&1; then fail "uninstall removed a program without a receipt"; fi
[ -e "$bin" ] || fail "the program without a receipt was removed"
rm "$bin"
mkdir "$bin"
if out=$(run --version 0.0.0 2>&1); then fail "a folder named fastwarp was accepted: $out"; fi
printf '%s' "$out" | grep -q 'is a folder' || fail "no message about the folder: $out"
rmdir "$bin"
run --version 0.0.0 >/dev/null 2>&1 || fail "install failed"
printf 'changed' >>"$bin"
if run --version 0.0.0 >/dev/null 2>&1; then fail "install replaced a program that no longer matches the receipt"; fi
rm "$bin"
run --uninstall >/dev/null 2>&1 || fail "uninstall with the program gone failed"
[ ! -e "$home/.local/bin" ] || fail "uninstall left the folder"
[ "$(markers "$home/.profile")" = 0 ] || fail "uninstall with the program gone left the PATH line"
pass "foreign fastwarp (symbolic link, script, no receipt, changed, folder) left alone"

cp "$home/.bashrc" "$work/bashrc.before"
run --version 0.0.0 >/dev/null 2>&1 || fail "install failed"
grep 'fastwarp-installer$' "$home/.profile" >"$work/own-line"
run --uninstall >/dev/null 2>&1 || fail "uninstall failed"
cat "$work/own-line" >>"$home/.bashrc"
run --version 0.0.0 >/dev/null 2>&1 || fail "install with a PATH line already in .bashrc failed"
grep -q ":$home/.bashrc\$" "$home/.local/bin/.fastwarp-installer" && fail "the receipt claims the line that was already in .bashrc"
run --uninstall >/dev/null 2>&1 || fail "uninstall failed"
[ "$(markers "$home/.bashrc")" = 1 ] || fail "uninstall removed a PATH line the installer did not add"
[ "$(markers "$home/.profile")" = 0 ] || fail "uninstall left the .profile line"
cp "$work/bashrc.before" "$home/.bashrc"
pass "a PATH line that was there before the install is kept"

env -i PATH="$base_path" TMPDIR="$work" FASTWARP_DOWNLOAD_URL="$dl" sh "$installer" --version 0.0.0 --dir "$work/nohome" >"$work/log" 2>&1 \
    || fail "install without HOME failed: $(cat "$work/log")"
grep -q 'HOME is not set' "$work/log" || fail "no note that HOME is not set"
env -i PATH="$base_path" TMPDIR="$work" sh "$installer" --uninstall --dir "$work/nohome" >/dev/null 2>&1 || fail "uninstall without HOME failed"
[ ! -e "$work/nohome" ] || fail "uninstall without HOME left the folder"
pass "HOME not set: --dir works, profiles untouched"

test_shell=/usr/bin/zsh
test_env=ZDOTDIR=$work/missing-zdotdir
run --version 0.0.0 >"$work/log" 2>&1 || fail "install for zsh with a missing ZDOTDIR failed"
grep -q 'could not write' "$work/log" && fail "the installer tried to write into the missing ZDOTDIR: $(cat "$work/log")"
[ ! -e "$work/missing-zdotdir" ] || fail "the installer created ZDOTDIR"
run --uninstall >/dev/null 2>&1 || fail "uninstall failed"
test_env=FASTWARP_TEST_UNUSED=1
run --version 0.0.0 >/dev/null 2>&1 || fail "install for zsh failed"
[ "$(markers "$home/.zshrc")" = 1 ] || fail "no PATH line in .zshrc"
test_shell=/usr/bin/fish
run --version 0.0.0 >/dev/null 2>&1 || fail "install for fish failed"
test_shell=/bin/bash
fish_conf=$home/.config/fish/conf.d/fastwarp-installer.fish
[ "$(markers "$fish_conf")" = 1 ] || fail "no fish conf.d file"
[ "$(markers "$home/.zshrc")" = 1 ] || fail "the second install duplicated the .zshrc line"
test_shell=/usr/bin/fish
run --version 0.0.0 --dir "$work/second" >/dev/null 2>&1 || fail "install of a second folder for fish failed"
[ "$(markers "$fish_conf")" = 2 ] || fail "the second folder replaced the fish line of the first"
test_shell=/bin/bash
run --uninstall >/dev/null 2>&1 || fail "uninstall failed"
[ "$(markers "$home/.zshrc")" = 1 ] || fail "uninstall of one folder removed the .zshrc line of the other"
[ "$(markers "$fish_conf")" = 1 ] || fail "uninstall of one folder removed the fish line of the other"
run --uninstall --dir "$work/second" >/dev/null 2>&1 || fail "uninstall of the second folder failed"
[ "$(markers "$home/.zshrc")" = 0 ] || fail "uninstall left a .zshrc line"
[ ! -e "$fish_conf" ] || fail "uninstall left the fish conf.d file"
pass "zsh and fish profiles, two folders in fish"

mkdir -p "$work/real"
ln -s "$work/real" "$work/link"
run --version 0.0.0 --dir "$work/link" >"$work/log" 2>&1 || fail "install through a linked folder failed"
run --uninstall --dir "$work/link" >"$work/log" 2>&1 || fail "uninstall through a linked folder failed"
grep -q 'still holds other files' "$work/log" && fail "uninstall claimed other files in an empty linked folder"
[ -L "$work/link" ] || fail "uninstall removed the link"
[ "$(markers "$home/.profile")" = 0 ] || fail "uninstall through a linked folder left a PATH line"
run --version 0.0.0 --dir "$work/real" >/dev/null 2>&1 || fail "install into the real folder failed"
run --version 0.0.0 --dir "$work/link" >/dev/null 2>&1 || fail "reinstall through the link failed"
[ "$(markers "$home/.profile")" = 2 ] || fail "expected one PATH line per spelling of the folder"
run --uninstall --dir "$work/real" >"$work/log" 2>&1 || fail "uninstall from the real folder failed: $(cat "$work/log")"
[ "$(markers "$home/.profile")" = 0 ] || fail "uninstall left the PATH line of the other spelling"
rm -f "$work/link"
rm -rf "$work/real"
pass "R1: a folder installed under two spellings is cleaned by one uninstall"

run --version 0.0.0 --dir "$work/first" >/dev/null 2>&1 || fail "install failed"
mkdir -p "$work/copy"
cp "$work/first/.fastwarp-installer" "$work/copy/"
run --uninstall --dir "$work/copy" >"$work/log" 2>&1 || fail "uninstall of a copied receipt failed: $(cat "$work/log")"
grep -q 'was written for' "$work/log" || fail "no warning about the copied receipt: $(cat "$work/log")"
[ "$(markers "$home/.profile")" = 1 ] || fail "a copied receipt removed the PATH line of the original folder"
[ ! -e "$work/copy/.fastwarp-installer" ] || fail "the copied receipt was kept"
run --uninstall --dir "$work/first" >/dev/null 2>&1 || fail "uninstall failed"
[ "$(markers "$home/.profile")" = 0 ] || fail "uninstall left the PATH line"
pass "R2: a receipt copied into another folder does not touch the original's PATH lines"

run --version 0.0.0 >/dev/null 2>&1 || fail "install failed"
rm -f "$bin"
ln -s /bin/true "$bin"
run --uninstall >"$work/log" 2>&1 || fail "uninstall after the program was replaced failed: $(cat "$work/log")"
grep -q 'was replaced since the install' "$work/log" || fail "no warning about the replaced program"
[ -L "$bin" ] || fail "uninstall removed the program that replaced fastwarp"
[ ! -e "$home/.local/bin/.fastwarp-installer" ] || fail "uninstall kept the receipt"
[ "$(markers "$home/.profile")" = 0 ] && [ "$(markers "$home/.bashrc")" = 0 ] || fail "uninstall kept the recorded PATH lines"
rm -rf "$home/.local/bin"
pass "R3: a program replaced since the install is kept; the recorded PATH lines and the receipt go"

chmod 444 "$home/.bashrc"
run --version 0.0.0 >"$work/log" 2>&1 || fail "install with a read-only .bashrc failed: $(cat "$work/log")"
chmod 644 "$home/.bashrc"
grep -q "could not write $home/.bashrc" "$work/log" || fail "no warning about the read-only .bashrc"
grep -q "^posix:.*:$home/.profile\$" "$home/.local/bin/.fastwarp-installer" || fail "the receipt lacks .profile"
grep -q ":$home/.bashrc\$" "$home/.local/bin/.fastwarp-installer" && fail "the receipt lists the .bashrc line that could not be written"
run --uninstall >/dev/null 2>&1 || fail "uninstall failed"
[ "$(markers "$home/.profile")" = 0 ] || fail "uninstall left the .profile line"
pass "R4: the receipt records only PATH lines that were written"

mkdir -p "$work/nocurl"
for tool in /usr/bin/* /bin/*; do
    name=${tool##*/}
    [ "$name" = curl ] || [ -e "$work/nocurl/$name" ] || [ -L "$work/nocurl/$name" ] || ln -s "$tool" "$work/nocurl/$name"
done
rm -f "$work/nocurl/wget"
printf '#!/bin/sh\nexit 1\n' >"$work/nocurl/wget"
chmod +x "$work/nocurl/wget"
test_path=$work/nocurl
dl=https://example.invalid/fastwarp
if out=$(run --version 0.0.0 2>&1); then fail "wget was used without a held checksum"; fi
printf '%s' "$out" | grep -q 'curl is needed' || fail "no message about curl: $out"
dl=file://$work/good
test_path=$base_path
pass "wget refused without a held checksum"

mkdir -p "$work/fake"
printf '#!/bin/sh\necho SunOS\n' >"$work/fake/uname"
chmod +x "$work/fake/uname"
test_path=$work/fake:$base_path
if out=$(run --version 0.0.0 2>&1); then fail "an unsupported system was accepted"; fi
test_path=$base_path
printf '%s' "$out" | grep -q 'unsupported operating system' || fail "no unsupported-system message: $out"
pass "unsupported operating system refused"

if out=$(run --version '0.0.0;rm' 2>&1); then fail "an invalid version was accepted"; fi
printf '%s' "$out" | grep -q 'invalid version' || fail "no invalid-version message: $out"
pass "invalid version refused"

if out=$(run --version 0.0.0 --dir "$work/a:b" 2>&1); then fail "a folder with ':' was accepted"; fi
printf '%s' "$out" | grep -q 'must not contain' || fail "no message about the folder name: $out"
pass "install folder with ':' refused"

for dl in http://192.0.2.1/fastwarp http://localhost:1@192.0.2.1/fastwarp; do
    if out=$(run --version 0.0.0 2>&1); then fail "plain HTTP without a held checksum was accepted: $dl"; fi
    printf '%s' "$out" | grep -q 'refusing to download over plain HTTP' || fail "no plain-HTTP message for $dl: $out"
done
dl=file://$work/good
pass "plain HTTP refused unless the checksum is held in the installer"

good_sum=$(sha256 "$work/good/$archive")
other_sum=$(printf '%064d' 1)
sed -e "s/@FASTWARP_VERSION@/0.0.0/" -e "s/@FASTWARP_SHA256_$target@/$good_sum/" \
    -e "s/@FASTWARP_SHA256_[a-z0-9_-]*@/$other_sum/" "$installer" >"$work/stamped.sh"
grep -q '@FASTWARP_' "$work/stamped.sh" && fail "the stamped installer still holds a placeholder"
script=$work/stamped.sh
dl=file://$work/bad
run >"$work/log" 2>&1 || fail "the stamped installer did not use its own checksum: $(cat "$work/log")"
grep -q 'held in this installer' "$work/log" || fail "the stamped installer did not report its checksum"
[ "$("$bin")" = "fastwarp 0.0.0-test" ] || fail "the stamped installer installed something else"
run --uninstall >/dev/null 2>&1 || fail "uninstall failed"
mkdir -p "$work/tampered" "$work/pkg/fastwarp-$target/extra"
tar -C "$work/pkg" -czf "$work/tampered/$archive" "fastwarp-$target"
printf '%s  %s\n' "$(sha256 "$work/tampered/$archive")" "$archive" >"$work/tampered/$archive.sha256"
dl=file://$work/tampered
if out=$(run 2>&1); then fail "a replaced archive with a matching .sha256 was accepted: $out"; fi
printf '%s' "$out" | grep -q 'checksum mismatch' || fail "no checksum message for the replaced archive: $out"
[ ! -e "$bin" ] || fail "the replaced archive was installed"
script=$installer
run --version 0.0.0 >/dev/null 2>&1 || fail "the repository installer refused the release's own .sha256"
run --uninstall >/dev/null 2>&1 || fail "uninstall failed"
dl=file://$work/good
pass "the stamped installer checks against its own checksum, not the release's .sha256"

run --help | grep -q -- '--no-modify-path' || fail "--help does not list --no-modify-path"
pass "--help"

echo "all installer tests passed"
