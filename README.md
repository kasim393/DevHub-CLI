# 🚀 DevHub CLI

A powerful, interactive PowerShell-based environment manager for multi-repo projects. DevHub simplifies your workflow by centralizing the management of frontend and backend services, allowing you to launch, explore, and debug with just a few keystrokes.

![DevHub CLI Preview](ss1.png)
![DevHub CLI Preview](ss2.png)

## ✨ Features

- **Interactive Menu:** Navigate through your repositories using arrow keys.
- **One-Click Start:** Launch all backend services or individual apps in new terminal windows.
- **Live Git Branches:** Every repo shows its current branch inline, e.g. `api-core  [feature/login]`. Press `R` to refresh all.
- **Process Tracking:** DevHub remembers what it started — a **"Stop all started processes"** option appears whenever something is running and kills the full process tree (terminal + npm + node).
- **System Health Check:** On startup, DevHub validates that `git`, `npm`, and `code` are on your PATH and that every configured repo path exists — with clear warnings if not.
- **Deep Integration:**
  - `V` — open any repository in **VS Code**
  - `E` — open the folder in **File Explorer**
  - `B` — check the current **Git branch**
- **Config-Driven:** All repos, categories, and apps live in a separate data file — customize everything without touching the script.
- **Lightweight:** Pure PowerShell (5.1 and 7.x), fast and responsive.

## 🛠️ Installation

1. **Clone the repository:**

   ```bash
   git clone https://github.com/your-username/devhub-cli.git
   cd devhub-cli
   ```

2. **Run it:**

   Double-click `start.bat`, or run the script directly:

   ```powershell
   ./dev-manager.ps1
   ```

   > `start.bat` handles execution policy for you and works whether you have PowerShell 7 (`pwsh`) or only Windows PowerShell 5.1. If you run the `.ps1` directly after downloading (rather than cloning) and hit a "not digitally signed" error, run `Unblock-File ./dev-manager.ps1` once.

## ⚙️ Configuration

Everything lives in **`dev-manager.config.psd1`** next to the script — pure data, no code.

### 1. Repositories

Each repo needs a `Name`, `Path`, `Start` command, and `Category`:

```powershell
Repos = @(
    @{ Name = 'api-core';     Path = 'C:\Projects\backend\api-core';  Start = 'npm start';   Category = 'Backend' }
    @{ Name = 'main-web-app'; Path = 'C:\Projects\frontend\main-web'; Start = 'npm run dev'; Category = 'Frontend' }
)
```

The `Start` command is per-repo, so mixing `npm start`, `npm run dev`, `yarn dev`, `dotnet run`, or anything else is trivial.

### 2. Categories

Categories become main-menu entries automatically. Add as many as you like:

```powershell
Categories = @(
    @{ Name = 'Backend';  Title = '🖥️  BACKEND SERVICES'; IncludeStartAll = $true }
    @{ Name = 'Frontend'; Title = '🌐 FRONTEND APPS';     IncludeStartAll = $false }
)
```

### 3. Other Tools & Apps

Launch any EXE or PATH command from the main menu:

```powershell
Apps = @(
    @{ Name = 'Notepad'; Path = 'notepad.exe' }
)
```

### 💡 Team tip

Commit a `dev-manager.config.psd1.template` and gitignore the real config — every teammate keeps their own local paths (C:\ vs D:\, etc.) without merge conflicts.

## ⌨️ Hotkeys

| Key       | Action                                  |
| --------- | --------------------------------------- |
| `↑` / `↓` | Navigate menu                           |
| `Enter`   | Launch / Select                         |
| `V`       | Open in VS Code (repos only)            |
| `E`       | Open in File Explorer (repos only)      |
| `B`       | Check & refresh Git branch (repos only) |
| `R`       | Refresh all branch labels               |
| `Esc`     | Back to main menu / Exit                |

### Adding your own hotkey

Actions are data. Add one entry to `$RepoActions` in `dev-manager.ps1` and the help line, key handling, and dispatch all update automatically:

```powershell
@{
    Name = 'Pull'; Char = 'G'; Hint = 'G = Git Pull'; RequiresRepo = $true
    Handler = { param($Repo) git -C $Repo.Path pull; Pause }
}
```

## 🚀 Future Roadmap

- [x] **System Health Check:** Startup validation for `git`, `npm`, and `code` commands.
- [ ] **Git Pull:** `G` hotkey to pull the highlighted repo (or pull all) before starting your day.
- [ ] **Open in Browser:** Launch `http://localhost:<port>` for running frontends with one keystroke.
- [ ] **Profiles:** Start a named group of repos (e.g. "auth stack") with one keystroke.
- [ ] **Port Awareness:** Show `[running :3000]` next to repos with an active dev server.

---

Created with ❤️ for developers who love efficiency.
