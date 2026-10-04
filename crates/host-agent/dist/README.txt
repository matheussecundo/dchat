dchat-host: let people you allow in dchat control this computer
=================================================================

While you share your entire screen in a dchat voice lounge, members can ask to
control your mouse and keyboard (one at a time) or to play with a game
controller (up to four). Nothing happens unless you click Allow in dchat.

Run it
------
  Windows:  double-click dchat-host.exe
  Linux:    ./install.sh once (asks for your password), then ./dchat-host

If it asks which dchat site may connect, type your site's address (for example
https://chat.example.com). Builds made for your site don't ask. You can also
give it on the command line: --allow-origin https://your-dchat-site

It prints a one-time code. In dchat, on this same computer: join voice, share
your entire screen, tap the mouse button, type the code and Connect. Keep the
dchat-host window open while others control this computer.

Stop at any time: Ctrl+Alt+Shift+Q (Windows and X11), Enter in this terminal,
Ctrl+C, or Stop in dchat. Everything held down is released.

Linux setup (once)
------------------
dchat-host creates virtual input devices through /dev/uinput. ./install.sh lets
whoever is logged in at this computer do that (it installs
60-dchat-host.rules and uinput.conf under /etc). ./install.sh --uninstall undoes
it. If dchat-host still says it has no permission, log out and back in once.

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
