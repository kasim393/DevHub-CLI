<#
.SYNOPSIS
    DevHub CLI — interactive environment manager for multi-repo projects.

.DESCRIPTION
    All configuration (repos, categories, apps) lives in
    dev-manager.config.psd1 next to this script. The script itself is
    generic: one menu engine, one repo menu, actions defined as data.
    Add a hotkey by adding one entry to $RepoActions below.

.NOTES
    Keys inside a repo menu:
        Enter = start the repo's dev command in a new terminal
        V     = open in VS Code
        E     = open in File Explorer
        B     = show current git branch (also refreshes it)
        R     = refresh all cached branch labels
        Esc   = back
#>

#Requires -Version 5.1
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# ============================================================
# CONFIG
# ============================================================
$script:ConfigPath = Join-Path $PSScriptRoot 'dev-manager.config.psd1'

function Import-DevConfig {
    if (-not (Test-Path -LiteralPath $script:ConfigPath)) {
        Write-Host "❌ Config file not found: $script:ConfigPath" -ForegroundColor Red
        Write-Host "   Create it next to this script and run again." -ForegroundColor Yellow
        exit 1
    }
    Import-PowerShellDataFile -Path $script:ConfigPath
}

$script:Config           = Import-DevConfig
$script:BranchCache      = @{}   # path -> branch name (refreshed on demand)
$script:StartedProcesses = New-Object System.Collections.ArrayList

# ============================================================
# HELPERS
# ============================================================
function Resolve-Command {
    <# Returns the first command name from $Candidates that exists on PATH, else $null. #>
    param([Parameter(Mandatory)][string[]]$Candidates)
    foreach ($candidate in $Candidates) {
        if (Get-Command $candidate -ErrorAction SilentlyContinue) { return $candidate }
    }
    return $null
}

function Get-CurrentGitBranch {
    param([Parameter(Mandatory)][string]$RepoPath)
    if (-not (Test-Path -LiteralPath $RepoPath)) { return 'not found' }
    try {
        $branch = git -C $RepoPath branch --show-current 2>$null
        if ($branch) { return ([string]$branch).Trim() }
        return 'unknown'
    }
    catch {
        return 'error'
    }
}

function Get-CachedBranch {
    param(
        [Parameter(Mandatory)][string]$RepoPath,
        [switch]$Refresh
    )
    if ($Refresh -or -not $script:BranchCache.ContainsKey($RepoPath)) {
        $script:BranchCache[$RepoPath] = Get-CurrentGitBranch -RepoPath $RepoPath
    }
    return $script:BranchCache[$RepoPath]
}

function Start-RepoTerminal {
    <# Launches the repo's Start command in a new terminal and tracks the process. #>
    param([Parameter(Mandatory)][hashtable]$Repo)

    $shell = Resolve-Command @('pwsh', 'powershell')
    $cmd   = "Set-Location -LiteralPath '$($Repo.Path)'; $($Repo.Start)"
    $proc  = Start-Process $shell -ArgumentList '-NoExit', '-Command', $cmd -PassThru

    [void]$script:StartedProcesses.Add(@{ Name = $Repo.Name; Process = $proc })
    Write-Host "▶️  Started '$($Repo.Name)' ($($Repo.Start))." -ForegroundColor Green
}

function Stop-StartedProcesses {
    <# Kills every process tree started from this session (pwsh + npm + node). #>
    if ($script:StartedProcesses.Count -eq 0) {
        Write-Host 'ℹ️ Nothing was started from this session.' -ForegroundColor Yellow
        return
    }
    foreach ($entry in $script:StartedProcesses) {
        $proc = $entry.Process
        try {
            if ($proc -and -not $proc.HasExited) {
                taskkill /PID $proc.Id /T /F 2>$null | Out-Null
                Write-Host "🛑 Stopped '$($entry.Name)' (PID $($proc.Id))." -ForegroundColor Yellow
            }
        }
        catch {
            Write-Host "⚠️ Could not stop '$($entry.Name)': $_" -ForegroundColor Red
        }
    }
    $script:StartedProcesses.Clear()
}

function Get-RunningStartedProcesses {
    # The leading comma stops PowerShell unrolling the array, so .Count is
    # always safe at call sites — even when the array is empty.
    return , @($script:StartedProcesses | Where-Object {
        $p = $_.Process
        $p -and -not $p.HasExited
    })
}

function Test-RepoPath {
    <# Prints an error and returns $false if the repo path is missing. #>
    param([Parameter(Mandatory)][hashtable]$Repo)
    if (Test-Path -LiteralPath $Repo.Path) { return $true }
    Write-Host "❌ Path not found: $($Repo.Path)" -ForegroundColor Red
    Pause
    return $false
}

# ============================================================
# ACTIONS (data-driven — add a hotkey here, everything updates)
# ============================================================
$script:RepoActions = @(
    @{
        Name = 'VSCode'; Char = 'V'; Hint = 'V = VS Code'; RequiresRepo = $true
        Handler = {
            param($Repo)
            $code = Resolve-Command @('code')
            if ($code) {
                Start-Process $code -ArgumentList $Repo.Path
            }
            else {
                Write-Host "⚠️ VS Code CLI not found. Run 'Shell Command: Install code command' in VS Code." -ForegroundColor Red
                Pause
            }
        }
    }
    @{
        # To add your own editor/IDE hotkey, copy this block and change
        # Name/Char/Hint and the command list passed to Resolve-Command.
        Name = 'Explorer'; Char = 'E'; Hint = 'E = Explorer'; RequiresRepo = $true
        Handler = { param($Repo) explorer $Repo.Path }
    }
    @{
        Name = 'Branch'; Char = 'B'; Hint = 'B = Git Branch'; RequiresRepo = $true
        Handler = {
            param($Repo)
            $branch = Get-CachedBranch -RepoPath $Repo.Path -Refresh
            Write-Host "🔍 Current branch for '$($Repo.Name)': $branch" -ForegroundColor Magenta
            Pause
        }
    }
    @{
        Name = 'Refresh'; Char = 'R'; Hint = 'R = Refresh branches'; RequiresRepo = $false
        Handler = { param($Repo) $script:BranchCache.Clear() }
    }
)

# ============================================================
# GENERIC MENU ENGINE
# ============================================================
function Show-Menu {
    <#
        Renders a navigable menu. $Items are hashtables with at least a Label key.
        Returns @{ Action = <name>; Item = <selected item or $null> }.
        Action is 'Select', 'Back', or the Name of a matched entry from $Actions.
    #>
    param(
        [Parameter(Mandatory)][object[]]$Items,
        [Parameter(Mandatory)][string]$Title,
        [object[]]$Actions = @()
    )

    $selectedIndex = 0
    $total = $Items.Count

    $hintParts = @('↑↓ = Navigate', 'Enter = Select')
    foreach ($action in $Actions) { $hintParts += $action.Hint }
    $hintParts += 'Esc = Back'
    $hint = $hintParts -join ' | '

    while ($true) {
        Clear-Host
        Write-Host $Title -ForegroundColor Cyan
        Write-Host ('=' * $Title.Length) -ForegroundColor Cyan
        Write-Host ''
        Write-Host $hint -ForegroundColor DarkGray
        Write-Host ''

        for ($i = 0; $i -lt $total; $i++) {
            if ($i -eq $selectedIndex) {
                Write-Host "  → $($Items[$i].Label)" -ForegroundColor Green
            }
            else {
                Write-Host "    $($Items[$i].Label)"
            }
        }

        $key = $Host.UI.RawUI.ReadKey('NoEcho, IncludeKeyDown')

        if ($key.VirtualKeyCode -eq 38) {                      # Up
            $selectedIndex = [Math]::Max(0, $selectedIndex - 1)
        }
        elseif ($key.VirtualKeyCode -eq 40) {                  # Down
            $selectedIndex = [Math]::Min($total - 1, $selectedIndex + 1)
        }
        elseif ($key.VirtualKeyCode -eq 13) {                  # Enter
            return @{ Action = 'Select'; Item = $Items[$selectedIndex] }
        }
        elseif ($key.VirtualKeyCode -eq 27) {                  # Esc
            return @{ Action = 'Back'; Item = $null }
        }
        else {
            $char = [char]::ToUpperInvariant($key.Character)
            foreach ($action in $Actions) {
                if ($action.Char -eq $char) {
                    return @{ Action = $action.Name; Item = $Items[$selectedIndex] }
                }
            }
        }
    }
}

# ============================================================
# REPO MENU (one function for every category)
# ============================================================
function Get-ReposByCategory {
    param([Parameter(Mandatory)][string]$CategoryName)
    return , @($script:Config.Repos | Where-Object { $_.Category -eq $CategoryName })
}

function New-RepoMenuItems {
    param(
        [object[]]$Repos = @(),
        [bool]$IncludeStartAll
    )

    $maxName = 0
    foreach ($repo in $Repos) {
        if ($repo.Name.Length -gt $maxName) { $maxName = $repo.Name.Length }
    }

    $items = New-Object System.Collections.ArrayList
    if ($IncludeStartAll -and $Repos.Count -gt 0) {
        [void]$items.Add(@{ Type = 'StartAll'; Label = 'Start all' })
    }
    if ((Get-RunningStartedProcesses).Count -gt 0) {
        [void]$items.Add(@{ Type = 'StopAll'; Label = 'Stop all started processes' })
    }
    foreach ($repo in $Repos) {
        $branch = Get-CachedBranch -RepoPath $repo.Path
        $label  = '{0}  [{1}]' -f $repo.Name.PadRight($maxName), $branch
        [void]$items.Add(@{ Type = 'Repo'; Label = $label; Repo = $repo })
    }
    [void]$items.Add(@{ Type = 'Back'; Label = '← Back to Main' })
    return , $items.ToArray()
}

function Show-CategoryMenu {
    param([Parameter(Mandatory)][hashtable]$Category)

    while ($true) {
        # Rebuilt each pass so branch labels and the Stop-all row stay current.
        $repos  = Get-ReposByCategory -CategoryName $Category.Name
        $items  = New-RepoMenuItems -Repos $repos -IncludeStartAll $Category.IncludeStartAll
        $result = Show-Menu -Items $items -Title $Category.Title -Actions $script:RepoActions

        if ($result.Action -eq 'Back') { return }
        $item = $result.Item

        # Global actions (e.g. Refresh) work no matter which row is highlighted.
        $globalAction = $script:RepoActions |
            Where-Object { $_.Name -eq $result.Action -and -not $_.RequiresRepo } |
            Select-Object -First 1
        if ($globalAction) {
            & $globalAction.Handler $null
            continue
        }

        switch ($item.Type) {
            'Back' {
                if ($result.Action -eq 'Select') { return }
            }
            'StartAll' {
                if ($result.Action -eq 'Select') {
                    foreach ($repo in $repos) {
                        if (Test-Path -LiteralPath $repo.Path) {
                            Start-RepoTerminal -Repo $repo
                        }
                        else {
                            Write-Host "❌ Path not found: $($repo.Path)" -ForegroundColor Red
                        }
                    }
                    Write-Host "✅ All $($Category.Name) repos started." -ForegroundColor Green
                    Pause
                }
                else {
                    Write-Host "ℹ️ 'Start all' has no single repo — highlight a repo first." -ForegroundColor Yellow
                    Pause
                }
            }
            'StopAll' {
                if ($result.Action -eq 'Select') {
                    Stop-StartedProcesses
                    Pause
                }
            }
            'Repo' {
                $repo = $item.Repo
                if (-not (Test-RepoPath -Repo $repo)) { break }

                if ($result.Action -eq 'Select') {
                    Start-RepoTerminal -Repo $repo
                    Start-Sleep -Milliseconds 500
                }
                else {
                    $action = $script:RepoActions |
                        Where-Object { $_.Name -eq $result.Action } |
                        Select-Object -First 1
                    if ($action) { & $action.Handler $repo }
                }
            }
        }
    }
}

# ============================================================
# APP LAUNCHER + MAIN MENU
# ============================================================
function Start-DevApp {
    param([Parameter(Mandatory)][hashtable]$App)

    $isFile    = Test-Path -LiteralPath $App.Path
    $isCommand = [bool](Get-Command $App.Path -ErrorAction SilentlyContinue)

    if ($isFile -or $isCommand) {
        Write-Host "🚀 Launching $($App.Name)..." -ForegroundColor Green
        Start-Process -FilePath $App.Path
        Start-Sleep -Seconds 1
    }
    else {
        Write-Host "❌ Application or command not found: $($App.Path)" -ForegroundColor Red
        Pause
    }
}

function Show-MainMenu {
    while ($true) {
        $items = New-Object System.Collections.ArrayList
        foreach ($category in $script:Config.Categories) {
            [void]$items.Add(@{
                Type     = 'Category'
                Label    = "Manage $($category.Name.ToUpper())"
                Category = $category
            })
        }
        foreach ($app in $script:Config.Apps) {
            [void]$items.Add(@{ Type = 'App'; Label = $app.Name; App = $app })
        }
        if ((Get-RunningStartedProcesses).Count -gt 0) {
            [void]$items.Add(@{ Type = 'StopAll'; Label = 'Stop all started processes' })
        }
        [void]$items.Add(@{ Type = 'Exit'; Label = 'Exit' })

        $result = Show-Menu -Items $items.ToArray() -Title '🚀 DEV HUB MANAGER'

        if ($result.Action -eq 'Back') { return }
        if ($result.Action -ne 'Select') { continue }

        switch ($result.Item.Type) {
            'Category' { Show-CategoryMenu -Category $result.Item.Category }
            'App'      { Start-DevApp -App $result.Item.App }
            'StopAll'  { Stop-StartedProcesses; Pause }
            'Exit'     { return }
        }
    }
}

# ============================================================
# SYSTEM HEALTH CHECK (startup validation)
# ============================================================
function Test-Environment {
    $checks = @(
        @{ Name = 'git';  Why = 'git branch detection (B / inline labels)' }
        @{ Name = 'npm';  Why = 'starting services (Enter / Start all)' }
        @{ Name = 'code'; Why = "opening repos in VS Code (V)" }
    )

    $warnings = New-Object System.Collections.ArrayList
    foreach ($check in $checks) {
        if (-not (Resolve-Command @($check.Name))) {
            [void]$warnings.Add("  ⚠️ '$($check.Name)' not found on PATH — needed for $($check.Why).")
        }
    }

    $missingRepos = New-Object System.Collections.ArrayList
    foreach ($repo in $script:Config.Repos) {
        if (-not (Test-Path -LiteralPath $repo.Path)) {
            [void]$missingRepos.Add("  ⚠️ '$($repo.Name)' path not found: $($repo.Path)")
        }
    }

    if ($warnings.Count -gt 0 -or $missingRepos.Count -gt 0) {
        Clear-Host
        Write-Host '🩺 SYSTEM HEALTH CHECK' -ForegroundColor Yellow
        Write-Host ('=' * 22) -ForegroundColor Yellow
        if ($warnings.Count -gt 0) {
            Write-Host ''
            Write-Host 'Missing commands:' -ForegroundColor Yellow
            foreach ($w in $warnings) { Write-Host $w -ForegroundColor Yellow }
        }
        if ($missingRepos.Count -gt 0) {
            Write-Host ''
            Write-Host "Missing repo paths (edit dev-manager.config.psd1):" -ForegroundColor Yellow
            foreach ($m in $missingRepos) { Write-Host $m -ForegroundColor Yellow }
        }
        Write-Host ''
        Write-Host 'You can continue — affected features will show errors when used.' -ForegroundColor DarkGray
        Pause
    }
}

# ============================================================
# ENTRY POINT
# ============================================================
Test-Environment
Show-MainMenu

Clear-Host
$stillRunning = Get-RunningStartedProcesses
if ($stillRunning.Count -gt 0) {
    Write-Host "ℹ️ $($stillRunning.Count) started process(es) are still running — left untouched." -ForegroundColor Yellow
}
Write-Host '👋 Goodbye! Happy coding!' -ForegroundColor Green
Start-Sleep -Seconds 1
