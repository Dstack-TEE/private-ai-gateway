@echo off
setlocal
set "PRIVATE_AI_PROXY_ALIAS=%~n0"
"%~dp0private-ai-proxy.exe" %*
