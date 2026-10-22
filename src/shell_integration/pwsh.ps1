# tether shell integration for PowerShell 7 (pwsh) and Windows PowerShell 5.1.
#
# Marks prompts and commands with OSC 133 (A prompt start, B prompt end, C command
# start, D;<exit> command end), reports the folder with OSC 7 and the git branch and
# command line with OSC 6973. The existing prompt (oh-my-posh, starship, posh-git, a
# custom one) and PSConsoleHostReadLine (PSReadLine) are wrapped, never replaced; $?
# and $LASTEXITCODE are handed to the original prompt unchanged.
#
# Loaded after the user's profile. Does nothing outside tether or when loaded twice.

if (-not $env:TETHER_SESSION -or (Test-Path variable:global:__tether_si)) { return }

$global:__tether_si = @{
    E = [char]27
    A = [char]7
    Prompt = $function:prompt
    ReadLine = $null
    # A command was read since the last prompt (so the next prompt reports its exit).
    Ran = $false
    Host = [System.Net.Dns]::GetHostName()
}

function global:__tether_osc([string]$Body) {
    $s = $global:__tether_si
    $s.E + ']' + $Body + $s.A
}

# `\` -> `\\`, `;` and control characters -> `\xNN` (see shell_integration::escape).
function global:__tether_escape([string]$Text) {
    $sb = New-Object System.Text.StringBuilder ($Text.Length + 8)
    foreach ($c in $Text.ToCharArray()) {
        $n = [int]$c
        if ($c -eq [char]92) { [void]$sb.Append('\\') }
        elseif ($c -eq [char]59 -or $n -lt 32 -or $n -eq 127) { [void]$sb.Append(('\x{0:x2}' -f $n)) }
        else { [void]$sb.Append($c) }
    }
    $sb.ToString()
}

# Branch of the repository holding the current folder, read from .git/HEAD (no git
# process). Empty outside a repository.
function global:__tether_branch([string]$Dir) {
    while ($Dir) {
        $dotgit = [System.IO.Path]::Combine($Dir, '.git')
        $gitdir = $null
        if ([System.IO.Directory]::Exists($dotgit)) { $gitdir = $dotgit }
        elseif ([System.IO.File]::Exists($dotgit)) {
            # Worktrees and submodules: ".git" is a file naming the real directory.
            $line = [System.IO.File]::ReadAllText($dotgit).Trim()
            if ($line.StartsWith('gitdir: ')) {
                $gitdir = $line.Substring(8)
                if (-not [System.IO.Path]::IsPathRooted($gitdir)) { $gitdir = [System.IO.Path]::Combine($Dir, $gitdir) }
            }
        }
        if ($gitdir) {
            $headFile = [System.IO.Path]::Combine($gitdir, 'HEAD')
            if (-not [System.IO.File]::Exists($headFile)) { return '' }
            $head = [System.IO.File]::ReadAllText($headFile).Trim()
            if ($head.StartsWith('ref: refs/heads/')) { return $head.Substring(16) }
            if ($head.StartsWith('ref: ')) { return $head.Substring(5) }
            if ($head.Length -ge 7) { return $head.Substring(0, 7) }
            return ''
        }
        $parent = [System.IO.Path]::GetDirectoryName($Dir)
        if (-not $parent -or $parent -eq $Dir) { break }
        $Dir = $parent
    }
    ''
}

# OSC 7 wants a file:// URL: forward slashes, '%', spaces and control characters encoded.
function global:__tether_cwd_url([string]$Path) {
    $p = $Path.Replace('\', '/').Replace('%', '%25').Replace(' ', '%20')
    if (-not $p.StartsWith('/')) { $p = '/' + $p }
    'file://' + $global:__tether_si.Host + $p
}

# Wraps PSConsoleHostReadLine so the command line and the C mark go out as soon as the
# user presses Enter (after PSReadLine has echoed the newline).
function global:__tether_hook_readline {
    $s = $global:__tether_si
    if ($s.ReadLine) { return }
    $current = Get-Command PSConsoleHostReadLine -CommandType Function -ErrorAction Ignore
    if (-not $current -or $current.ScriptBlock.ToString().Contains('__tether_si')) { return }
    $s.ReadLine = $current.ScriptBlock
    # Tether sends Meta+Enter for Shift+Enter (also understood by agent TUIs).
    if (Get-Command Set-PSReadLineKeyHandler -ErrorAction Ignore) {
        Set-PSReadLineKeyHandler -Chord 'Alt+Enter' -Function AddLine
        Set-PSReadLineKeyHandler -Chord 'Shift+Enter' -Function AddLine
    }
    function global:PSConsoleHostReadLine {
        $s = $global:__tether_si
        $line = & $s.ReadLine
        if ($line -and $line.Trim()) {
            [Console]::Write((__tether_osc ('6973;cmd;' + (__tether_escape $line))) + (__tether_osc '133;C'))
            $s.Ran = $true
        }
        $line
    }
}

function global:prompt {
    # Read these before anything else runs.
    $ok = $global:?
    $code = $global:LASTEXITCODE
    $s = $global:__tether_si
    $marks = ''
    if ($s.Ran) {
        $exit = 0
        if (-not $ok) { if ($code -is [int] -and $code -ne 0) { $exit = $code } else { $exit = 1 } }
        $marks += __tether_osc "133;D;$exit"
        $s.Ran = $false
    }
    $loc = $ExecutionContext.SessionState.Path.CurrentLocation
    if ($loc.Provider.Name -eq 'FileSystem') {
        $dir = $loc.ProviderPath
        $marks += __tether_osc ('7;' + (__tether_cwd_url $dir))
        $branch = ''
        try { $branch = __tether_branch $dir } catch { }
        $marks += __tether_osc ('6973;branch;' + (__tether_escape $branch))
    }
    $marks += __tether_osc '133;A'
    [Console]::Write($marks)
    __tether_hook_readline

    # The original prompt sees the user's $? and $LASTEXITCODE.
    $global:LASTEXITCODE = $code
    if (-not $ok) { Write-Error '' -ErrorAction Ignore }
    $text = & $s.Prompt
    if ($null -eq $text) { $text = '' }
    $text = $text -join ''
    # Rows the prompt takes, to place marks that ConPTY delivers ahead of the text.
    $rows = $text.Split([char]10).Length
    $text + (__tether_osc "6973;prompt_rows;$rows") + (__tether_osc '133;B')
}
