# Packaging

Templates for the package managers that install a published release. Nothing
here runs in CI; after publishing a release, render the files for it:

```bash
packaging/render.sh 0.4.11     # needs gh; fills packaging/out/ (git-ignored)
```

The installers' SHA-256 comes from the release itself, so nothing is downloaded.

## winget

`out/winget/<version>/` is a complete manifest set (package id
`paintballrefjosh.Linecard`). Check it with `winget validate --manifest <dir>`
and test it with `winget install --manifest <dir>` (enable local manifests
once with `winget settings --enable LocalManifestFiles`).

Publish by copying the folder to
`manifests/p/paintballrefjosh/Linecard/<version>/` in a fork of
[microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs) and opening a
pull request. After the first version is accepted, `wingetcreate update
paintballrefjosh.Linecard --version <v> --urls <installer url> --submit` does
the same in one step.

## Homebrew

`out/homebrew/linecard.rb` is a cask. Homebrew's own repository wants
notable projects, so use a tap: create a public repository named
`homebrew-linecard`, put the file at `Casks/linecard.rb`, and users install with

```bash
brew install --cask paintballrefjosh/linecard/linecard
```

The cask sets `auto_updates true` (the app updates itself) and a `livecheck`
that follows the latest GitHub release. The macOS build is unsigned, so the
first launch needs right-click → Open, as the README says.
