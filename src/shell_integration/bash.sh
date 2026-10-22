# tether shell integration for bash (4.0+; Git Bash, WSL, Linux, macOS).
#
# Marks prompts and commands with OSC 133 (A prompt start, B prompt end, C command
# start, D;<exit> command end), reports the folder with OSC 7 and the git branch and
# command line with OSC 6973. PS1 and PROMPT_COMMAND set by the user (or starship,
# oh-my-bash, bash-preexec) keep working: our hooks run first and last around them,
# and $? is handed on unchanged. The prompt path starts no processes (forks are slow
# on Git Bash); reading the command line costs one `history` capture per command.
#
# Sourced after the user's rc files. Does nothing outside tether, in non-interactive
# shells, or when sourced twice.

if [[ -n "${TETHER_SESSION-}" && -z "${__tether_si-}" && $- == *i* ]]; then
__tether_si=1

# Sets __tether_out to $1 with `\` -> `\\`, `;` and control characters -> `\xNN`
# (see shell_integration::escape).
__tether_escape() {
    # Quoted replacements are literal in every bash version (no backslash or `&` magic).
    local b='\'
    local s=${1//"$b"/"$b$b"}
    s=${s//;/"${b}x3b"}
    s=${s//$'\n'/"${b}x0a"}
    s=${s//$'\r'/"${b}x0d"}
    s=${s//$'\t'/"${b}x09"}
    s=${s//$'\e'/"${b}x1b"}
    s=${s//$'\a'/"${b}x07"}
    __tether_out=$s
}

# Sets __tether_out to the branch from .git/HEAD of the enclosing repository (no git
# process); empty outside one.
__tether_branch() {
    __tether_out=
    local dir=$PWD gitdir= head=
    while [[ -n "$dir" ]]; do
        if [[ -d "$dir/.git" ]]; then
            gitdir=$dir/.git
            break
        elif [[ -f "$dir/.git" ]]; then
            # Worktrees and submodules: ".git" is a file naming the real directory.
            read -r head < "$dir/.git"
            gitdir=${head#gitdir: }
            [[ "$gitdir" == /* ]] || gitdir=$dir/$gitdir
            break
        fi
        [[ "$dir" == / ]] && break
        dir=${dir%/*}
        [[ -z "$dir" ]] && dir=/
    done
    [[ -n "$gitdir" && -r "$gitdir/HEAD" ]] || return 0
    head=
    read -r head < "$gitdir/HEAD"
    case $head in
        "ref: refs/heads/"*) __tether_out=${head#ref: refs/heads/} ;;
        "ref: "*) __tether_out=${head#ref: } ;;
        *) __tether_out=${head:0:7} ;;
    esac
}

__tether_ran=
__tether_at_prompt=
__tether_last_hist=
__tether_pwd=
__tether_winpwd=

# First in PROMPT_COMMAND: report how the last command ended, the folder and branch.
__tether_precmd() {
    local code=$?
    __tether_at_prompt=
    if [[ -n "$__tether_ran" ]]; then
        printf '\e]133;D;%s\a' "$code"
        __tether_ran=
    fi
    local cwd=$PWD
    if [[ -n "${MSYSTEM-}" ]]; then
        # Git Bash / MSYS2: report the Windows path (C:/...), computed once per folder.
        if [[ "$PWD" != "$__tether_pwd" ]]; then
            __tether_pwd=$PWD
            __tether_winpwd=$(builtin pwd -W 2>/dev/null) || __tether_winpwd=$PWD
        fi
        cwd=/$__tether_winpwd
    fi
    cwd=${cwd//%/%25}
    __tether_branch
    __tether_escape "$__tether_out"
    printf '\e]7;file://%s%s\a\e]6973;branch;%s\a' "${HOSTNAME-}" "${cwd// /%20}" "$__tether_out"
    return $code
}

# Last in PROMPT_COMMAND: wrap whatever PS1 is now (prompt frameworks rebuild it).
__tether_postcmd() {
    local code=$?
    if [[ "$PS1" != *'133;B'* ]]; then
        PS1='\[\e]133;A\a\]'$PS1'\[\e]133;B\a\]'
    fi
    # Rows the prompt takes (`\n` escapes and real newlines), to place marks that
    # ConPTY delivers ahead of the text.
    local t=${PS1//\\n/$'\n'}
    t=${t//[^$'\n']/}
    printf '\e]6973;prompt_rows;%s\a' "$(( ${#t} + 1 ))"
    __tether_at_prompt=1
    return $code
}

# From the DEBUG trap: the first command after a prompt is what the user typed.
__tether_preexec() {
    [[ -n "$__tether_at_prompt" ]] || return 0
    [[ "$BASH_COMMAND" == __tether_* ]] && return 0
    __tether_at_prompt=
    local line
    line=$(HISTTIMEFORMAT= builtin history 1)
    # A line kept out of history (ignorespace) leaves the previous entry: use the command.
    if [[ "$line" == "$__tether_last_hist" ]]; then
        line=$BASH_COMMAND
    else
        __tether_last_hist=$line
        line=${line#*[0-9] }
        line=${line# }
    fi
    __tether_escape "$line"
    printf '\e]6973;cmd;%s\a\e]133;C\a' "$__tether_out"
    __tether_ran=1
}

# Insert a literal newline in Readline without accepting the command.
bind '"\e\r": "\C-v\C-j"' 2>/dev/null
__tether_last_hist=$(HISTTIMEFORMAT= builtin history 1)

if [[ -n "${bash_preexec_imported-}${__bp_imported-}" ]]; then
    # bash-preexec owns the DEBUG trap: use its hook arrays.
    precmd_functions=(__tether_precmd "${precmd_functions[@]}" __tether_postcmd)
    preexec_functions+=(__tether_preexec)
else
    # Keep (and still run) a DEBUG trap someone else installed.
    __tether_old_debug=$(trap -p DEBUG)
    __tether_old_debug=${__tether_old_debug#trap -- \'}
    __tether_old_debug=${__tether_old_debug%\' DEBUG}
    __tether_old_debug=${__tether_old_debug//\'\\\'\'/\'}
    __tether_debug() {
        local code=$?
        __tether_preexec
        if [[ -n "$__tether_old_debug" ]]; then
            (exit $code)
            eval "$__tether_old_debug"
        fi
    }
    trap '__tether_debug' DEBUG
    if [[ "$(declare -p PROMPT_COMMAND 2>/dev/null)" == "declare -a"* ]]; then
        PROMPT_COMMAND=(__tether_precmd "${PROMPT_COMMAND[@]}" __tether_postcmd)
    else
        # Newlines, not ';': the user's value may already end in one.
        PROMPT_COMMAND=__tether_precmd$'\n'${PROMPT_COMMAND:+$PROMPT_COMMAND$'\n'}__tether_postcmd
    fi
fi
fi
