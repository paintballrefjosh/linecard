# Releasing

## One-time: update-signing key

The in-app updater only installs updates signed with the project's key. This is free and separate from Windows/macOS code signing (which we're skipping for now).

```powershell
cd app
npx tauri signer generate -w $HOME/.tauri/linecard.key
```

1. Put the printed **public key** in `app/src-tauri/tauri.conf.json` → `plugins.updater.pubkey`.
2. In the GitHub repo, add secrets `TAURI_SIGNING_PRIVATE_KEY` (contents of the `.key` file) and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.
3. Back up the private key somewhere safe. If it's lost, existing installs can't verify future updates and users must reinstall manually.

## Each release

1. Bump `version` in `Cargo.toml` (workspace) and `app/src-tauri/tauri.conf.json`; add a `CHANGELOG.md` entry.
2. Tag and push: `git tag v0.2.0 && git push --tags`.
3. The Release workflow builds Windows (NSIS), macOS (universal dmg, plus the `.app.tar.gz` the updater installs) and Linux (AppImage, deb), uploads them to a **draft** release with `latest.json`, and pushes `ghcr.io/<owner>/linecard-relay`.
4. Review the draft and publish it. Installed apps see it within six hours (or on next launch), download in the background, and offer "Restart to install". A tag with a hyphen (`v0.3.0-beta.1`) is marked pre-release.

## Without code signing

- Windows SmartScreen warns on the first install ("More info → Run anyway"). In-app updates afterwards don't trigger it.
- macOS needs right-click → Open on first launch.
