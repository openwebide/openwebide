# Project site

The GitHub Pages site has its own landing page in `site/index.md`. Documentation
is generated directly from `docs/**/*.md`, `README.md` and `CHANGELOG.md`; edit those
files with the code they describe. The build makes a temporary snapshot and
copies supporting assets such as CSV files.
There are no maintained copies of the documentation and no separate wiki.

## Documentation structure

[Documentation](index.md) is the entry point. The navigation groups guides into
Getting started, Using Open WebIDE, Hosting and administration, Development, and
Project. Each group begins with an overview linking its guides; existing guide
filenames stay in place so published URLs and repository links remain usable.

Edit `site/navigation.yml` to place a new page in the appropriate section. The
builder requires every Markdown page to appear exactly once and rejects missing
files, duplicate entries and unlisted pages. Section names and link labels are
curated independently of page headings. The sidebar expands the current section,
and breadcrumbs link back to its overview.

Keep step-by-step user guides in Getting started or Using Open WebIDE, installation
and operations in Hosting, and implementation references in Development. Link
related material across sections rather than duplicating it. Update the section
overview when adding a guide.

The site uses MkDocs with a small custom theme in `site/theme/`. Its palette and
button foundations are extracted from `frontend/styles.css` during the build;
site layout styles live in `site/assets/site.css`. Light and dark appearance follow
the device preference without persisting browser settings. Mermaid code fences
render using a pinned external renderer, with readable source as a fallback.

Flowcharts use a pinned ELK layout plugin, with Mermaid's default layout as a
fallback if the plugin cannot load. Sequence diagrams use their own renderer.
Mermaid fences use the `mermaid` language tag. The deferred loader works on hosted
pages and local `file://` previews; rendering requires access to jsDelivr. Diagrams
follow the device's light/dark preference when the page loads.

## Build and preview

Install the pinned Rust toolchain, the `wasm32-unknown-unknown` target and Trunk
0.21.14 (as for app development), then run from the repository root:

```sh
python3 -m venv /tmp/openwebide-pages-venv
/tmp/openwebide-pages-venv/bin/pip install -r site/requirements.txt
/tmp/openwebide-pages-venv/bin/python -m unittest discover -s site -p 'test_*.py'
/tmp/openwebide-pages-venv/bin/python site/build.py --output /tmp/openwebide-site
python3 -m http.server 8000 --directory /tmp/openwebide-site
```

Open <http://localhost:8000/>. Builds run in strict mode and check generated HTML,
so missing documentation links, anchors and local assets fail the build. Keep
generated output outside the repository; the build only replaces directories it
previously generated, to avoid deleting unrelated files.
Markdown links between source documents remain ordinary relative `.md` links;
MkDocs converts them to site URLs. Images should use Markdown image syntax for
relative-path rewriting. Keep raw HTML image paths relative to the generated page.

## Publishing

In the repository's **Settings → Pages → Build and deployment**, set **Source**
to **GitHub Actions**. `.github/workflows/pages.yml` validates builds on pull
requests, then publishes site changes on `main`; it can also run manually.
The canonical address is <https://openwebide.com/>. Set **Custom domain** to
`openwebide.com` in the same Pages settings. With an Actions deployment, GitHub
uses this setting rather than a repository `CNAME` file.

At the DNS provider, replace the registrar's parking/redirect records for `@`
and `www` with these records:

| Type | Host | Value |
| --- | --- | --- |
| A | `@` | `185.199.108.153` |
| A | `@` | `185.199.109.153` |
| A | `@` | `185.199.110.153` |
| A | `@` | `185.199.111.153` |
| CNAME | `www` | `openwebide.github.io` |

GitHub redirects `www` to the configured apex domain. Enable **Enforce HTTPS**
once GitHub has provisioned its certificate. See
[GitHub's custom-domain guide](https://docs.github.com/en/pages/configuring-a-custom-domain-for-your-github-pages-site/managing-a-custom-domain-for-your-github-pages-site)
for DNS configuration and propagation checks. To build for another deployment,
pass `--site-url https://example.com/` to the build command.

The build compiles the standalone [panel playground](panel-playground.md) using
the app's Rust/WASM renderer and copies its bundle into the site. It does not
launch the app backend or connect to a bridge. To reuse an existing Trunk bundle,
pass `--playground-dist /path/to/bundle`; the bundle must include `index.html`
and a WASM module. Visitors can use it without an account or workspace.
With a compatible ChromeDriver available, check the compiled tool with
`CHROMEDRIVER=/path/to/chromedriver python3 tools/check-panel-playground.py --site /tmp/openwebide-site`.
This runs a disposable headless browser and checks both responsive themes,
downloads and the embedded renderer without accessing personal browser profiles.
The site links GitHub Issues for feedback and support.
Issue forms cover bugs, feature requests and setup/usage help. An optional demo
and launch announcements remain part of the 1.0 public-release work.

## Social profiles

- [Bluesky](https://bsky.app/profile/did:plc:vw3hdjhj2253v3hc6cjtt4ys): account DID
  `did:plc:vw3hdjhj2253v3hc6cjtt4ys`. The site uses this stable profile URL so handle
  changes do not break links. For `@openwebide.com`, the DNS TXT record at `_atproto`
  must be `did=did:plc:vw3hdjhj2253v3hc6cjtt4ys`; verify it in Bluesky's handle settings.
- [Mastodon](https://mastodon.social/@openwebide): `@openwebide@mastodon.social`.
  Put `https://openwebide.com/` in a profile metadata field named Website. The site
  footer links back with `rel="me"`, allowing Mastodon to verify that website field
  once the site is deployed. Save the Mastodon profile again after deployment if
  verification has not appeared.

  The discovery alias `@openwebide@openwebide.com` is served by
  `site/static/.well-known/webfinger`. Its subject stays
  `acct:openwebide@mastodon.social`, so clients resolve to the existing account;
  the displayed handle stays `@openwebide@mastodon.social`. GitHub Pages serves
  one static response regardless of query parameters; this is a single-account
  alias rather than a general WebFinger service. Mastodon parses its JSON body,
  but clients requiring a specific response content type may not support it.
  The Pages workflow packages `.well-known` explicitly; the convenience Pages
  upload action excludes dot directories.
- [YouTube](https://www.youtube.com/@OpenWebIDE): `@OpenWebIDE`, channel ID
  `UCRRWQeyGWL1kkG0vjNIFtzA`. Demos, setup walkthroughs, and feature updates.
  The editable [banner source](assets/youtube-banner.svg) and
  [upload-ready PNG](assets/youtube-banner.png) use the app logo and palette.

The [social header source](assets/social-banner.svg) and
[upload-ready PNG](assets/social-banner.png) provide matching Bluesky and Mastodon
headers. Keep these assets alongside the YouTube banner when updating branding.

Use GitHub Issues for support and bug reports; social profiles are for project
updates and discovery.

## Link previews and acknowledgements

Every page includes Open Graph and Twitter large-image card metadata, with its
own title, description and canonical URL. The shared preview uses
`site/assets/social-card.png` (1200 × 630); its editable source is the adjacent
SVG. The homepage canonical URL is the domain root.

The [open source software page](open-source-software.md) combines its repository
Markdown introduction with an inventory generated by `site/credits.py` using the app's shared
`tools/open_source.py` Cargo and bundled-asset inventory during
site builds. Cargo metadata uses `--locked --all-features` to include resolved
third-party crates across the workspace, including platform, development,
benchmark and vendored dependencies. The inventory reports package versions,
upstream links and declared licenses, with direct dependencies listed first.
The site builder also inventories MkDocs and its installed Python dependencies.

Generated tables only exist in the build output; there is no second list to edit.
Pages rebuilds when Cargo manifests or the lockfile change. CI uses stable Cargo
for metadata without compiling the app or installing WASM targets.
