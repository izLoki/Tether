# tether shell integration for zsh.
#
# Marks prompts and commands with OSC 133 (A prompt start, B prompt end, C command
# start, D;<exit> command end), reports the folder with OSC 7 and the git branch and
# command line with OSC 6973. Uses add-zsh-hook, so oh-my-zsh, powerlevel10k, starship
# and friends keep their own precmd/preexec hooks; PS1 is wrapped (again) after any
# theme rebuilt it, and $? is left alone.
#
# Sourced after the user's .zshrc. Does nothing outside tether, in non-interactive
# shells, or when sourced twice.

if [[ -n "${TETHER_SESSION-}" && -z "${__tether_si-}" && -o interactive ]]; then
typeset -g __tether_si=1 __tether_ran= __tether_out=

# Sets __tether_out to $1 with `\` -> `\\`, `;` and control characters -> `\xNN`
# (see shell_integration::escape).
__tether_escape() {
    local b='\'
    local s=${1//$b/$b$b}
    s=${s//;/${b}x3b}
    s=${s//$'\n'/${b}x0a}
    s=${s//$'\r'/${b}x0d}
    s=${s//$'\t'/${b}x09}
    s=${s//$'\e'/${b}x1b}
    s=${s//$'\a'/${b}x07}
    __tether_out=$s
}

# Sets __tether_out to the branch from .git/HEAD of the enclosing repository (no git
# process); empty outside one.
__tether_branch() {
    __tether_out=
    local dir=$PWD gitdir= head=
    while [[ -n $dir ]]; do
        if [[ -d $dir/.git ]]; then
            gitdir=$dir/.git
            break
        elif [[ -f $dir/.git ]]; then
            read -r head < $dir/.git
            gitdir=${head#gitdir: }
            [[ $gitdir == /* ]] || gitdir=$dir/$gitdir
            break
        fi
        [[ $dir == / ]] && break
        dir=${dir%/*}
        [[ -z $dir ]] && dir=/
    done
    [[ -n $gitdir && -r $gitdir/HEAD ]] || return 0
    head=
    read -r head < $gitdir/HEAD
    case $head in
        "ref: refs/heads/"*) __tether_out=${head#ref: refs/heads/} ;;
        "ref: "*) __tether_out=${head#ref: } ;;
        *) __tether_out=${head[1,7]} ;;
    esac
}

__tether_precmd() {
    local code=$?
    if [[ -n $__tether_ran ]]; then
        printf '\e]133;D;%s\a' $code
        __tether_ran=
    fi
    local cwd=${PWD//\%/%25}
    __tether_branch
    __tether_escape "$__tether_out"
    printf '\e]7;file://%s%s\a\e]6973;branch;%s\a' "${HOST-}" "${cwd// /%20}" "$__tether_out"
    return $code
}

__tether_preexec() {
    __tether_escape "$1"
    printf '\e]6973;cmd;%s\a\e]133;C\a' "$__tether_out"
    __tether_ran=1
}

autoload -Uz add-zsh-hook
__tether_add_line() { LBUFFER+=$'\n' }
zle -N __tether_add_line
bindkey $'\e\r' __tether_add_line
add-zsh-hook precmd __tether_precmd
add-zsh-hook preexec __tether_preexec
# Run first among precmd hooks so $? is still the command's, and wrap PS1 last: themes
# that rebuild PS1 in their own precmd are wrapped again by the line below.
precmd_functions=(__tether_precmd ${precmd_functions:#__tether_precmd} __tether_rewrap)
__tether_rewrap() {
    local code=$?
    if [[ $PS1 != *'133;B'* ]]; then
        PS1=$'%{\e]133;A\a%}'$PS1$'%{\e]133;B\a%}'
    fi
    # Rows the prompt takes, to place marks that ConPTY (WSL) delivers ahead of the text.
    local t=${(%)PS1}
    t=${t//[^$'\n']/}
    printf '\e]6973;prompt_rows;%s\a' $(( ${#t} + 1 ))
    return $code
}
fi
