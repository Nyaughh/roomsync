@echo off
cd /d "%~dp0"
roomsync-desktop.exe wizard
if errorlevel 1 echo RoomSync stopped. Read the message above before retrying.
pause
