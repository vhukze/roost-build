
# Internal fixed-path action. Success stages disable markers, not runtime unload.
# No environment-selected paths, deletion, overwrite, grant or reboot.
base='/data/adb'
fail_disable() {
    printf 'LSPosed disable staging incomplete; earlier markers may already exist.\n' >&2
    exit 2
}
check_layout() {
    [ ! -L "$base" ] && [ -d "$base" ] && [ -r "$base" ] && [ -x "$base" ] || fail_disable
    ls -A "$base" >/dev/null || fail_disable
    for parent in "$base/modules" "$base/modules_update"; do
        [ ! -L "$parent" ] || fail_disable
        if [ -e "$parent" ]; then
            [ -d "$parent" ] && [ -r "$parent" ] && [ -x "$parent" ] || fail_disable
            ls -A "$parent" >/dev/null || fail_disable
            for name in zygisk_lsposed riru_lsposed; do
                dir="$parent/$name"
                [ ! -L "$dir" ] || fail_disable
                if [ -e "$dir" ]; then
                    [ -d "$dir" ] && [ -r "$dir" ] && [ -x "$dir" ] || fail_disable
                    ls -A "$dir" >/dev/null || fail_disable
                    for marker in disable remove; do
                        [ ! -L "$dir/$marker" ] || fail_disable
                        if [ -e "$dir/$marker" ]; then
                            [ -f "$dir/$marker" ] || fail_disable
                        fi
                    done
                fi
            done
        fi
    done
}
check_layout
for parent in "$base/modules" "$base/modules_update"; do
    for name in zygisk_lsposed riru_lsposed; do
        dir="$parent/$name"
        if [ -d "$dir" ]; then
            if [ ! -e "$dir/disable" ]; then
                # Noclobber rejects an existing file or link at creation time.
                (umask 022 && set -C && : > "$dir/disable") || fail_disable
            fi
            [ ! -L "$dir/disable" ] && [ -f "$dir/disable" ] || fail_disable
        fi
    done
done
check_layout
for parent in "$base/modules" "$base/modules_update"; do
    for name in zygisk_lsposed riru_lsposed; do
        dir="$parent/$name"
        if [ -d "$dir" ]; then
            [ ! -L "$dir/disable" ] && [ -f "$dir/disable" ] || fail_disable
        fi
    done
done
printf 'ROOST_LSPOSED_DISABLE_STAGED\n'
