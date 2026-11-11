# Terraform language tools

Install the versions in `versions.json` from the repository root:

```sh
python3 tools/setup-terraform-language-server.py
export PATH="$PWD/tools/terraform-language-server/installed/bin:$PATH"
export CHECKPOINT_DISABLE=1 TF_IN_AUTOMATION=1
cargo run -p openwebide-bridge --example check_terraform_server --locked
```

The installer supports macOS, Linux and Windows on amd64 and arm64. It checks
archive SHA-256 hashes and both reported versions before publishing an install.
`--cache DIRECTORY` can supply upstream ZIP files; cached files receive the same
checks. Use a fresh `--tool-path` when updating pins. Existing directories with
different metadata are preserved and rejected.

Pins come from the official [Terraform releases](https://releases.hashicorp.com/terraform/)
and [terraform-ls releases](https://releases.hashicorp.com/terraform-ls/).
Review each release's `SHA256SUMS` and update all supported platforms together.
Upstream license files are installed alongside the binaries. The application
image copies these tools and licenses from its dedicated tooling stage.

The probe uses temporary source files and unsaved buffers. It does not run
Terraform init, plan, apply or destroy, and it leaves source files unchanged.
Tool installation does not install or activate the independent language plugin.
