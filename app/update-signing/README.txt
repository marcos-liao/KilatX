KILATX UPDATE SIGNING KEY

update.key signs update.json (see src/update.rs). It is NOT in the repository (.gitignore):
the maintainer keeps it offline. Every installed KilatX only accepts updates signed with it.
Its public half is PUBLIC_KEY in src/update.rs
(1b19891592bda132cd8049827c37e2105ec5293b444381ff09e8f12eac97d246).

- Back it up offline (USB stick / password manager). If it is lost, installed copies can't
  be told about new versions any more (users would have to download manually once).
- If it leaks, anyone could offer a signed "update": make a new key, ship a version with
  the new PUBLIC_KEY, and stop using the old one.
- Forks: make your own key (sign-update keygen <folder>) and put its public half in
  PUBLIC_KEY, and point UPDATE_URL at your own releases.

Publishing a release (after uploading the setups to the GitHub release v0.5.1):
  sign-update sign update-signing\update.key 0.5.1 notes.txt ^
      https://github.com/marcos-liao/KilatX/releases/tag/v0.5.1 update.json ^
      --file x64   https://github.com/marcos-liao/KilatX/releases/download/v0.5.1/KilatX-Setup-0.5.1-x64.exe   ..\dist\0.5.1\x64\KilatX-Setup-0.5.1-x64.exe ^
      --file arm64 https://github.com/marcos-liao/KilatX/releases/download/v0.5.1/KilatX-Setup-0.5.1-arm64.exe ..\dist\0.5.1\arm64\KilatX-Setup-0.5.1-arm64.exe
Then attach update.json to the same release (KilatX reads releases/latest/download/update.json).
