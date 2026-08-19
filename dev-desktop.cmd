@echo off
REM Deprecated legacy desktop launcher. Default Anchor development is Web Admin + CLI.
cd /d "%~dp0"
echo [desktop] DEPRECATED: use "pnpm start" for Web Admin. This launcher is legacy-only.
if not exist "node_modules\@tauri-apps\cli" (
  echo Installing pnpm dependencies for the legacy desktop target...
  call corepack pnpm install
  if errorlevel 1 exit /b 1
)
call corepack pnpm legacy:desktop
