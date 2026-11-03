@echo off
cd /d "%~dp0"
roomsync-desktop.exe web
if errorlevel 1 echo RoomSync stopped. Read the message above before retrying.
pause
