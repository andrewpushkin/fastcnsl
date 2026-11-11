@echo off
rem Startup probe for bench.ps1: signals that the shell runs inside the
rem terminal, then waits for RUNDIR\quit and exits with 0.
echo started>"%~1\begin"
:wait
if exist "%~1\quit" exit /b 0
"%SystemRoot%\System32\ping.exe" -n 1 -w 100 192.0.2.1 >nul
goto wait
