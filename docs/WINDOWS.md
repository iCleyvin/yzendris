# Windows notes

## Administrator windows and UAC prompts

Windows isolates programs by integrity level (UIPI). A program running
normally can neither observe input aimed at an elevated window nor inject
input into one. For Yzendris this means:

- **Windows as host:** while an administrator window (Task Manager, an
  elevated terminal, an installer) has focus, the cursor will not cross to
  another computer and shortcuts are not seen. Click a normal window and it
  works again.
- **Windows as client:** the host can move the cursor over an administrator
  window but cannot click or type in it.

The straightforward fix is to run Yzendris itself elevated. Create a scheduled
task that starts it with highest privileges at sign-in (run once in an
administrator PowerShell):

```powershell
$exe = "$env:LOCALAPPDATA\Programs\Yzendris\yzendris.exe"
reg delete HKCU\Software\Microsoft\Windows\CurrentVersion\Run /v Yzendris /f
$action  = New-ScheduledTaskAction -Execute $exe -Argument 'run'
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME
$who     = New-ScheduledTaskPrincipal -UserId $env:USERNAME -LogonType Interactive -RunLevel Highest
Register-ScheduledTask -TaskName Yzendris -Action $action -Trigger $trigger -Principal $who -Force
```

Think before doing this: an elevated Yzendris means a paired host can type
into administrator windows on that PC.

UAC consent prompts themselves appear on a separate *secure desktop* that no
ordinary program — elevated or not — can reach. Answer those with the PC's own
keyboard, or lower the UAC level so prompts appear on the normal desktop.

### The `uiaccess` build (advanced)

Windows has a narrower mechanism than elevation, *UIAccess*, meant for
accessibility tools. `cargo build --release --features uiaccess` embeds a
manifest requesting it. Windows honours the request only if the executable is
Authenticode-signed by a certificate the machine trusts **and** installed
under `%ProgramFiles%`; otherwise such a binary refuses to start. That is why
it is not the default. If you sign your own builds this is the least-privilege
way to reach elevated windows.

## Firewall

The host listens on TCP 7547 (and answers discovery on UDP 7547 while you are
adding a device). `yzendris install` offers to add a rule for *private*
networks. If your network is marked *Public* in Windows, either change it to
Private or add the rule yourself.

## Display scaling

Yzendris declares itself per-monitor DPI aware and works in physical pixels,
so mixed scaling factors across monitors are handled.
