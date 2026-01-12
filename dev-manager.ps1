# dev-manager.ps1

$Backends = @{
    "api-core"      = "C:\Projects\my-app\backend\api-core"
    "auth-service"  = "C:\Projects\my-app\backend\auth-service"
    "worker-service" = "C:\Projects\my-app\backend\worker"
}

$Frontends = @{
    "main-web-app"  = "C:\Projects\my-app\frontend\main-web"
    "admin-panel"   = "C:\Projects\my-app\frontend\admin-panel"
}

# ====== Function to get current git branch ======
function Get-CurrentGitBranch {
    param([string]$RepoPath)
    
    if (Test-Path $RepoPath) {
        $currentDir = Get-Location
        Set-Location $RepoPath
        try {
            $branch = git branch --show-current 2>$null
            if ($branch) {
                return $branch.Trim()
            } else {
                return "unknown"
            }
        } catch {
            return "error"
        } finally {
            Set-Location $currentDir
        }
    }
    return "not found"
}

# ====== Reusable Menu Function ======
function Show-Menu {
    param(
        [string[]]$Items,
        [string]$Title
    )

    $selectedIndex = 0
    $total = $Items.Count

    while ($true) {
        Clear-Host
        Write-Host $Title -ForegroundColor Cyan
        Write-Host ("=" * $Title.Length) -ForegroundColor Cyan
        Write-Host ""
        Write-Host "↑↓ = Navigate | Enter = Select | V = Open in VS Code | E = Open in Explorer | B = Show Git Branch | Esc = Back" -ForegroundColor DarkGray
        Write-Host ""

        for ($i = 0; $i -lt $total; $i++) {
            if ($i -eq $selectedIndex) {
                Write-Host "  → $($Items[$i])" -ForegroundColor Green
            } else {
                Write-Host "    $($Items[$i])"
            }
        }

        $key = $Host.UI.RawUI.ReadKey("NoEcho, IncludeKeyDown")
        $keyCode = $key.VirtualKeyCode
        $keyChar = $key.Character

        # Navigation
        if ($keyCode -eq 38) { $selectedIndex = [Math]::Max(0, $selectedIndex - 1) }       # Up
        elseif ($keyCode -eq 40) { $selectedIndex = [Math]::Min($total - 1, $selectedIndex + 1) } # Down
        
        # Enter → select
        elseif ($keyCode -eq 13) { return @{ Action = "Select"; Index = $selectedIndex } }
        
        # Esc → back
        elseif ($keyCode -eq 27) { return @{ Action = "Back"; Index = $null } }
        
        # 'V' → open in VS Code
        elseif ($keyChar -eq 'v' -or $keyChar -eq 'V') { return @{ Action = "VSCode"; Index = $selectedIndex } }
        
        # 'E' → open in Explorer
        elseif ($keyChar -eq 'e' -or $keyChar -eq 'E') { return @{ Action = "Explorer"; Index = $selectedIndex } }
        
        # 'B' → show git branch
        elseif ($keyChar -eq 'b' -or $keyChar -eq 'B') { return @{ Action = "Branch"; Index = $selectedIndex } }
    }
}

function Show-BackendMenu {
    $names = @($Backends.Keys)
    $items = @("Start all backends") + $names + @("← Back to Main")

    while ($true) {
        $result = Show-Menu -Items $items -Title "🖥️  BACKEND SERVICES"
        
        # Handle "Back" (Esc or "← Back" selected)
        if ($result.Action -eq "Back") { return }
        if ($result.Index -eq ($items.Count - 1)) { return }

        $idx = $result.Index

        # "Start all backends" is index 0
        if ($idx -eq 0) {
            if ($result.Action -eq "Select") {
                # Start all backends (only on Enter)
                foreach ($name in $names) {
                    $path = $Backends[$name]
                    if (Test-Path $path) {
                        $cmd = "Set-Location -Path '$path'; npm start"
                        Start-Process pwsh -ArgumentList "-NoExit", "-Command", $cmd
                    }
                }
                Write-Host "✅ All backends started." -ForegroundColor Green
                Pause
            }
            elseif ($result.Action -eq "VSCode") {
                Write-Host "ℹ️ 'Start all' has no single folder to open." -ForegroundColor Yellow
                Pause
            }
            elseif ($result.Action -eq "Explorer") {
                Write-Host "ℹ️ 'Start all' has no single folder to open." -ForegroundColor Yellow
                Pause
            }
            elseif ($result.Action -eq "Branch") {
                Write-Host "ℹ️ 'Start all' has no single repository to check." -ForegroundColor Yellow
                Pause
            }
            continue
        }

        # Individual backend (index 1+ → maps to $names[$idx - 1])
        $name = $names[$idx - 1]
        $path = $Backends[$name]
        if (-not (Test-Path $path)) {
            Write-Host "❌ Path not found: $path" -ForegroundColor Red
            Pause
            continue
        }

        switch ($result.Action) {
            "Select" {
                # Start service in new terminal
                $cmd = "Set-Location -Path '$path'; npm start"
                Start-Process pwsh -ArgumentList "-NoExit", "-Command", $cmd
            }
            "VSCode" {
                # Open ONLY in VS Code — no terminal
                if (Get-Command code -ErrorAction SilentlyContinue) {
                    Start-Process code -ArgumentList $path
                } else {
                    Write-Host "⚠️ VS Code CLI not found. Run 'Shell Command: Install code command' in VS Code." -ForegroundColor Red
                    Pause
                }
            }
            "Explorer" {
                # Open ONLY in File Explorer
                explorer $path
            }
            "Branch" {
                # Show current git branch
                $branch = Get-CurrentGitBranch -RepoPath $path
                Write-Host "🔍 Current branch for '$name': $branch" -ForegroundColor Magenta
                Pause
            }
        }
    }
}

function Show-FrontendMenu {
    $names = @($Frontends.Keys)
    $items = $names + @("← Back to Main")

    while ($true) {
        $result = Show-Menu -Items $items -Title "🌐 FRONTEND APPS"
        
        if ($result.Action -eq "Back") { return }
        if ($result.Index -eq ($items.Count - 1)) { return }

        $idx = $result.Index
        $name = $names[$idx]
        $path = $Frontends[$name]

        if (-not (Test-Path $path)) { continue }

        switch ($result.Action) {
            "Select" {
                # This opens a terminal (expected)
                $cmd = "Set-Location -Path '$path'; npm run dev"
                Start-Process pwsh -ArgumentList "-NoExit", "-Command", $cmd
            }
            "VSCode" {
                # ✅ This opens ONLY VS Code — no terminal
                if (Test-Path $path) {
                    Start-Process code -ArgumentList $path
                }
            }
            "Explorer" {
                # ✅ This opens ONLY Explorer
                if (Test-Path $path) {
                    explorer $path
                }
            }
            "Branch" {
                # Show current git branch
                $branch = Get-CurrentGitBranch -RepoPath $path
                Write-Host "🔍 Current branch for '$name': $branch" -ForegroundColor Magenta
                Pause
            }
        }
    }
}

# ====== OTHER TOOLS/APPS CONFIG ======
$OtherApps = @{
    "Notepad"    = "notepad.exe"
    # To add more, just follow the format: "Display Name" = "Path to EXE or File"
}

# ====== MAIN MENU ======
function Show-MainMenu {
    while ($true) {
        $appNames = @($OtherApps.Keys)
        $mainItems = @("Manage BACKEND services", "Manage FRONTEND apps") + $appNames + @("Exit")
        
        $result = Show-Menu -Items $mainItems -Title "🚀 DEV HUB MANAGER"
        
        if ($result.Action -eq "Back") { return }
        
        $idx = $result.Index
        $totalItems = $mainItems.Count
        
        # 1. Manage Backends
        if ($idx -eq 0) { Show-BackendMenu }
        
        # 2. Manage Frontends
        elseif ($idx -eq 1) { Show-FrontendMenu }
        
        # 3. Handle Dynamic "Other Apps"
        elseif ($idx -ge 2 -and $idx -lt ($totalItems - 1)) {
            $appName = $appNames[$idx - 2]
            $appPath = $OtherApps[$appName]
            
            # Check if it's a file path or a system command
            if ((Test-Path $appPath) -or (Get-Command $appPath -ErrorAction SilentlyContinue)) {
                Write-Host "🚀 Launching $appName..." -ForegroundColor Green
                Start-Process -FilePath $appPath
                Start-Sleep -Seconds 1
            } else {
                Write-Host "❌ Application or Command not found: $appPath" -ForegroundColor Red
                Pause
            }
        }
        
        # 4. Exit
        elseif ($idx -eq ($totalItems - 1)) {
            Clear-Host
            Write-Host "👋 Goodbye! Happy coding!" -ForegroundColor Green
            Start-Sleep 1.5
            exit
        }
    }
}

# Start the application
Show-MainMenu