@{
    # ============================================================
    # DevHub CLI — Configuration
    # ------------------------------------------------------------
    # This file is plain data — no code runs from here.
    # Edit paths for your machine. Tip: commit a *.template copy
    # to git and gitignore this file so every teammate keeps
    # their own local paths.
    #
    # Every repo entry requires: Name, Path, Start, Category
    # Categories appear in the main menu automatically — add as
    # many as you like (e.g. 'Services', 'Tools', 'Legacy').
    # ============================================================

    Categories = @(
        @{ Name = 'Backend';  Title = '🖥️  BACKEND SERVICES'; IncludeStartAll = $true }
        @{ Name = 'Frontend'; Title = '🌐 FRONTEND APPS';     IncludeStartAll = $false }
    )

    Repos = @(
        # ---- Backend ----
        @{ Name = 'api-core';       Path = 'C:\Projects\my-app\backend\api-core';     Start = 'npm start';   Category = 'Backend' }
        @{ Name = 'auth-service';   Path = 'C:\Projects\my-app\backend\auth-service'; Start = 'npm start';   Category = 'Backend' }
        @{ Name = 'worker-service'; Path = 'C:\Projects\my-app\backend\worker';       Start = 'npm start';   Category = 'Backend' }

        # ---- Frontend ----
        @{ Name = 'main-web-app'; Path = 'C:\Projects\my-app\frontend\main-web';    Start = 'npm run dev'; Category = 'Frontend' }
        @{ Name = 'admin-panel';  Path = 'C:\Projects\my-app\frontend\admin-panel'; Start = 'npm run dev'; Category = 'Frontend' }
    )

    # Standalone tools/apps launched from the main menu.
    # Path may be a full path to an EXE/file, or a command on PATH.
    Apps = @(
        @{ Name = 'Notepad'; Path = 'notepad.exe' }
        # @{ Name = 'DB Manager'; Path = 'C:\Program Files\DBeaver\dbeaver.exe' }
    )
}
