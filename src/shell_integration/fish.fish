# tether shell integration for fish (3.1+).
#
# Marks prompts and commands with OSC 133 (A prompt start, B prompt end, C command
# start, D;<exit> command end), reports the folder with OSC 7 and the git branch and
# command line with OSC 6973. fish_prompt (tide, starship, a custom one) is wrapped,
# not replaced, and keeps seeing the command's $status.
#
# Run through `--init-command`, after the user's config. Does nothing outside tether,
# in non-interactive shells, or when loaded twice.

if set -q TETHER_SESSION; and not set -q __tether_si; and status is-interactive
    set -g __tether_si 1
    set -g __tether_ran

    # `\` -> `\\`, `;` and control characters -> `\xNN` (see shell_integration::escape).
    function __tether_escape
        set -l s (string replace -a -- '\\' '\\\\' $argv[1] | string collect)
        set s (string replace -a -- ';' '\\x3b' $s | string collect)
        set s (string replace -a -- \n '\\x0a' $s | string collect)
        set s (string replace -a -- \r '\\x0d' $s | string collect)
        set s (string replace -a -- \t '\\x09' $s | string collect)
        set s (string replace -a -- \e '\\x1b' $s | string collect)
        set s (string replace -a -- \a '\\x07' $s | string collect)
        printf '%s' $s
    end

    # Branch from .git/HEAD of the enclosing repository (no git process).
    function __tether_branch
        set -l dir $PWD
        set -l gitdir
        while test -n "$dir"
            if test -d "$dir/.git"
                set gitdir "$dir/.git"
                break
            else if test -f "$dir/.git"
                read -l line <"$dir/.git"
                set gitdir (string replace -r '^gitdir: ' '' -- $line)
                string match -q '/*' -- $gitdir; or set gitdir "$dir/$gitdir"
                break
            end
            test "$dir" = /; and break
            set dir (string replace -r '/[^/]*$' '' -- $dir)
            test -z "$dir"; and set dir /
        end
        test -n "$gitdir"; and test -r "$gitdir/HEAD"; or return 0
        read -l head <"$gitdir/HEAD"
        if string match -q 'ref: refs/heads/*' -- $head
            string replace 'ref: refs/heads/' '' -- $head
        else if string match -q 'ref: *' -- $head
            string replace 'ref: ' '' -- $head
        else
            string sub -l 7 -- $head
        end
    end

    function __tether_preexec --on-event fish_preexec
        printf '\e]6973;cmd;%s\a\e]133;C\a' (__tether_escape "$argv[1]")
        set -g __tether_ran 1
    end

    function __tether_postexec --on-event fish_postexec
        set -g __tether_status $status
    end

    function __tether_on_prompt --on-event fish_prompt
        if test -n "$__tether_ran"
            printf '\e]133;D;%s\a' $__tether_status
            set -g __tether_ran
        end
        set -l cwd (string replace -a '%' '%25' -- $PWD | string replace -a ' ' '%20')
        printf '\e]7;file://%s%s\a\e]6973;branch;%s\a' $hostname $cwd (__tether_escape (__tether_branch))
    end

    function __tether_set_status
        return $argv[1]
    end

    if functions -q fish_prompt
        functions -c fish_prompt __tether_user_prompt
    else
        function __tether_user_prompt
            printf '%s> ' $PWD
        end
    end
    bind \e\r 'commandline -i \n'
    function fish_prompt
        set -l code $status
        printf '\e]133;A\a'
        # Hand the command's status to the user's prompt.
        __tether_set_status $code
        __tether_user_prompt
        printf '\e]133;B\a'
    end
end
