# Releasing Malgel

Releases are built by [`.github/workflows/release.yml`](../.github/workflows/release.yml).
It runs when a `v*` tag is pushed, and can be started by hand from the
Actions tab (Release → Run workflow) to try the packaging without
publishing anything.

| Platform | Files | Built on |
| --- | --- | --- |
| macOS 11+ | `Malgel-<version>-macos-universal.dmg`: `Malgel.app` for Apple silicon and Intel, with an Applications shortcut | `macos-14` |
| Windows 10+ | `Malgel-<version>-windows-x64-setup.exe` (installer) and `Malgel-<version>-windows-x64.zip` (portable `Malgel.exe`) | `windows-latest` |
| Linux x86_64 | `Malgel-<version>-x86_64.AppImage` and `malgel-<version>-linux-x86_64.tar.gz` | `ubuntu-22.04` |

A tag push publishes a GitHub Release with all of these, a
`SHA256SUMS.txt` and the release notes. A manual run (**Actions → Release →
Run workflow**) does what its **publish** choice says:

- **draft** (the default): a test build on a draft release titled
  "Malgel <version> test build <run number>", under Releases and visible
  only to people who can push to the repository. Drafts never create a tag;
  delete it when you're done.
- **prerelease**: the same test build published for anyone to download,
  marked as a pre-release (never "Latest") with the tag
  `v<version>-build.<run number>`.
- **release**: the real release of the version in `Cargo.toml`, exactly as a
  tag push would make it. GitHub creates the tag `v<version>` on the commit
  the run built, so nothing needs pushing but `main`. A version that is
  already released is refused.

The macOS job attaches its disk image to the draft release itself rather
than passing it on as an artifact, so it still delivers a signed build
when GitHub-hosted runners or Actions artifact storage are unavailable.

## Cutting a release

1. Bump `version` in `Cargo.toml` and run `cargo build` so `Cargo.lock`
   picks it up.
2. Add a `<release version="X.Y.Z" date="YYYY-MM-DD"/>` line at the top of
   `<releases>` in `packaging/linux/dev.malgel.Malgel.metainfo.xml`.
3. Optionally write the release notes in `.github/release-notes/vX.Y.Z.md`;
   without that file the release lists the changes GitHub finds since the
   previous release.
4. Commit and push, then either push a tag or run the Release workflow with
   **publish: release**:

   ```sh
   git commit -am "Release X.Y.Z"
   git push origin main
   git tag -a vX.Y.Z -m "Malgel X.Y.Z" && git push origin vX.Y.Z   # or run the workflow
   ```

The tag must be `v` followed by the version in `Cargo.toml`; the workflow
stops otherwise. A version with a pre-release part (`v0.3.0-beta.1`) is
published as a pre-release.

The version shown in the app, the macOS `Info.plist`, the Windows file
properties and the installer all come from `Cargo.toml`.

## Code signing

Signing is optional. Each platform signs when its settings are present and
falls back otherwise, so a fork or a fresh repository still produces
working packages. macOS signing uses secrets; add them under **Settings →
Secrets and variables → Actions → New repository secret**, or with the
GitHub CLI (`gh secret set NAME < file`). The workflow never prints them.
Windows signing needs no secrets, only the repository variables described
under [Windows](#windows).

| Secret | Used for |
| --- | --- |
| `APPLE_CERTIFICATE` | Developer ID Application certificate and private key: a `.p12` file, base64-encoded |
| `APPLE_CERTIFICATE_PASSWORD` | Password of that `.p12` (leave it unset if the `.p12` has a blank password) |
| `APPLE_SIGNING_IDENTITY` | Certificate name, e.g. `Developer ID Application: Jane Doe (AB12CD34EF)` |
| `APPLE_API_KEY` | Notarization with an App Store Connect API key: the `.p8` file, base64-encoded |
| `APPLE_API_KEY_ID` | That key's ID |
| `APPLE_API_ISSUER` | Issuer ID shown above the key list |
| `APPLE_ID` | Notarization with an Apple ID instead: its email address |
| `APPLE_TEAM_ID` | 10-character Team ID |
| `APPLE_APP_SPECIFIC_PASSWORD` | App-specific password for that Apple ID |

### macOS

Signing needs the first three secrets; notarization additionally needs
either the three API key secrets or the three Apple ID secrets (the API
key is used when both are set). Everything requires a paid Apple Developer
Program membership.

**Developer ID certificate.** In Xcode, open Settings → Accounts, select
the team, choose Manage Certificates, and add a *Developer ID Application*
certificate (or create one at developer.apple.com → Certificates with a
certificate signing request from Keychain Access). Then, in Keychain
Access → login → My Certificates, right-click
*Developer ID Application: …* (it must have its private key underneath),
choose Export, save as `.p12` and pick a password.

```sh
base64 -i DeveloperID.p12 | pbcopy          # paste as APPLE_CERTIFICATE
security find-identity -v -p codesigning    # the name for APPLE_SIGNING_IDENTITY
```

**App Store Connect API key (recommended for notarization).** In App Store
Connect → Users and Access → Integrations → App Store Connect API, create a
Team Key with the Developer role and download `AuthKey_<ID>.p8` (it can be
downloaded only once). Store `base64 -i AuthKey_<ID>.p8` as
`APPLE_API_KEY`, the key ID as `APPLE_API_KEY_ID` and the Issuer ID as
`APPLE_API_ISSUER`.

**Or an Apple ID.** Create an app-specific password at account.apple.com →
Sign-In and Security → App-Specific Passwords. The Team ID is listed under
Membership details at developer.apple.com.

With a certificate, the workflow imports it into a temporary keychain,
signs `Malgel.app` with the hardened runtime and a secure timestamp
(Malgel needs no entitlements), then signs the disk image. With
notarization credentials it submits the app and the disk image to Apple's
notary service, waits for the result (printing Apple's log if it is
rejected) and staples the tickets, so Gatekeeper accepts Malgel even
offline. The keychain is deleted at the end of the job, whatever the
outcome.

### Other macOS runners

The macOS jobs in both CI and the release workflow run on GitHub's
`macos-14` runner unless the repository variable `MACOS_RUNNER` names
another (**Settings → Secrets and variables → Actions → Variables**). For
a third-party service such as [GetMac](https://getmac.io/github-runners),
install its GitHub App for this repository and set `MACOS_RUNNER` to the
runner label it gives you, e.g. `getmac`. The runner needs Xcode's
command-line tools (`codesign`, `xcrun notarytool`, `iconutil`, `lipo`) and
`hdiutil`; Rust is installed by the workflow.

CI builds on macOS only when it's run by hand (**Actions → CI → Run
workflow**), so pushes don't spend macOS runner minutes; the release
workflow always builds on macOS.

### Windows

Windows files are signed with [Azure Artifact
Signing](https://learn.microsoft.com/azure/artifact-signing/) (formerly
Trusted Signing): Microsoft verifies the publisher's identity once and keeps
the signing key, and GitHub Actions signs in with its own short-lived OIDC
token, so no password or certificate is stored in the repository. One
signing account and certificate profile can sign any number of projects.

1. In the Azure portal, create an Artifact Signing account, complete
   **identity validation** (choose *Public*), and create a certificate
   profile of type **Public Trust**. The validated name is the publisher
   Windows shows.
2. Let every run of the Release workflow sign in to Azure with the same
   identity, whether it was started by hand or by a tag. Azure matches that
   identity exactly, and GitHub's default one includes the branch or tag,
   so change it once for the repository to name the workflow instead:

   ```sh
   gh api -X PUT repos/rcawston/Malgel/actions/oidc/customization/sub \
     -F use_default=false \
     -f 'include_claim_keys[]=repo' -f 'include_claim_keys[]=workflow'
   ```

   Runs of the Release workflow then identify as
   `repo:rcawston/Malgel:workflow:Release`.
3. In Microsoft Entra ID, create an **app registration**, and under
   *Certificates & secrets → Federated credentials* add a credential with
   the scenario **Other issuer**: issuer
   `https://token.actions.githubusercontent.com`, subject
   `repo:rcawston/Malgel:workflow:Release`, audience
   `api://AzureADTokenExchange`. Other projects can reuse the app with a
   credential of their own.
4. On the signing account's **Access control (IAM)** page, assign that app
   the **Artifact Signing Certificate Profile Signer** role.
5. Add these as repository **variables** (Settings → Secrets and variables
   → Actions → Variables); none of them is secret:

| Variable | Value |
| --- | --- |
| `AZURE_TENANT_ID` | Directory (tenant) ID |
| `AZURE_CLIENT_ID` | The app registration's application (client) ID |
| `AZURE_SUBSCRIPTION_ID` | Subscription holding the signing account |
| `ARTIFACT_SIGNING_ENDPOINT` | The account's endpoint, e.g. `https://eus.codesigning.azure.net/` |
| `ARTIFACT_SIGNING_ACCOUNT` | The signing account's name |
| `ARTIFACT_SIGNING_PROFILE` | The certificate profile's name |

With them set, `Malgel.exe` (before it goes into the zip and the installer)
and the installer are signed with SHA-256, timestamped by Microsoft, and
checked. SmartScreen still warns about a new publisher until downloads
build up its reputation; signing makes that happen and names you as the
publisher rather than "Unknown publisher".

### Without secrets

- **macOS:** the app is signed ad hoc, so it runs on Apple silicon but
  Gatekeeper blocks it after download. Open it once by right-clicking
  `Malgel.app` → Open; on macOS 15 and later, try to open it, then choose
  System Settings → Privacy & Security → Open Anyway. Alternatively:
  `xattr -dr com.apple.quarantine /Applications/Malgel.app`.
- **Windows:** the files are unsigned. SmartScreen may show "Windows
  protected your PC"; choose More info → Run anyway.
- **Linux:** packages are never signed; nothing changes.

## What gets built

- **Icons.** `packaging/icon/malgel.svg` is the source artwork and
  `malgel-small.svg` a simplified version for 16–32 px. After editing either,
  run `packaging/icon/render.sh` (needs `rsvg-convert` and ImageMagick) and
  commit the regenerated PNGs, `packaging/macos/Malgel.iconset`,
  `packaging/windows/malgel.ico` and `packaging/linux/icons`.
- **macOS.** Release builds for `aarch64-apple-darwin` and
  `x86_64-apple-darwin` (with `MACOSX_DEPLOYMENT_TARGET=11.0`) are merged
  with `lipo`. `packaging/macos/bundle.sh` assembles `Malgel.app` from
  `packaging/macos/Info.plist`, which declares Malgel an editor for Markdown
  (`net.daringfireball.markdown`: `.md`, `.markdown`, `.mdown`, `.mkd`,
  `.mkdn`) and plain text, and converts the iconset with `iconutil`.
  `packaging/macos/notarize.sh` handles notarization.
- **Windows.** `build.rs` embeds the icon and version information in
  `malgel.exe` when the target is Windows (it does nothing elsewhere). The
  Inno Setup script `packaging/windows/malgel.iss` installs per user by
  default (all users on request), adds a Start menu entry, registers
  Malgel under "Open with" for `.md`, `.markdown`, `.mdown`, `.mkd` and
  `.txt`, and lists it in Default Apps without changing any existing
  default.
- **Linux.** `packaging/linux/package.sh` installs the binary, desktop
  entry, AppStream metadata and icons into a prefix and archives it; the
  tarball can be unpacked straight into `~/.local`. With `LINUXDEPLOY`
  set it also builds the AppImage, bundling the libraries that are not part
  of a base system. Building on Ubuntu 22.04 keeps the glibc requirement
  at 2.35.

Each script can be run locally on its platform; see the comment at its top.
