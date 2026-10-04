dchat-host: let people you allow in dchat control this computer
=================================================================

While you share your entire screen in a dchat voice lounge, members can ask to
control your mouse and keyboard (one at a time) or to play with a game
controller (up to four). Nothing happens unless you click Allow in dchat.

Run it
------
  Linux:    ./dchat-host --allow-origin https://your-dchat-site
  Windows:  dchat-host.exe --allow-origin https://your-dchat-site

(If this build was made with your site built in, --allow-origin is optional.)
It prints a one-time code. In dchat: join voice, share your entire screen, tap
the mouse button, type the code and Connect.

Stop at any time: Ctrl+Alt+Shift+Q (Windows and X11), Enter in this terminal,
Ctrl+C, or Stop in dchat. Everything held down is released.

Linux setup (once)
------------------
dchat-host creates virtual input devices through /dev/uinput:
  sudo cp 60-dchat-host.rules /etc/udev/rules.d/
  sudo cp uinput.conf /etc/modules-load.d/
  sudo modprobe uinput && sudo udevadm control --reload && sudo udevadm trigger
Then log out and back in.

Windows notes
-------------
- Controllers need the ViGEmBus driver:
  https://github.com/nefarius/ViGEmBus/releases
- Windows of apps running as administrator, UAC prompts and the lock screen can
  only be controlled when dchat-host runs as administrator too.
- This program is not code-signed: Windows SmartScreen may warn ("More info",
  then "Run anyway"). Check the download against SHA256SUMS.

Privacy
-------
dchat-host listens only on 127.0.0.1, accepts only your dchat site, and pairs
only with a tab that knows its one-time code. It connects nowhere else, writes
no files, and never logs what anyone types.
