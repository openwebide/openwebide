#!/usr/bin/env bash
set -euo pipefail

fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
export HOME="$fixture/home"
export XDG_DATA_HOME="$fixture/data"
export XDG_CACHE_HOME="$fixture/cache"
export DOTNET_CLI_HOME="$fixture/cli"
export NUGET_PACKAGES="$fixture/packages"
export DOTNET_CLI_TELEMETRY_OPTOUT=1
export DOTNET_GENERATE_ASPNET_CERTIFICATE=false
export DOTNET_CLI_WORKLOAD_UPDATE_NOTIFY_DISABLE=true
export DOTNET_NOLOGO=1

cd "$fixture"
mkdir Library Client "$HOME"
cat > Smoke.slnx <<'EOF'
<Solution><Project Path="Library/Library.csproj"/><Project Path="Client/Client.csproj"/></Solution>
EOF
cat > NuGet.Config <<'EOF'
<configuration><packageSources><clear/></packageSources></configuration>
EOF
cat > Library/Library.csproj <<'EOF'
<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup></Project>
EOF
cat > Client/Client.csproj <<'EOF'
<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup><ItemGroup><ProjectReference Include="../Library/Library.csproj"/></ItemGroup></Project>
EOF
cat > Library/Answer.cs <<'EOF'
namespace Library; public static class Answer { public static int Value => 42; }
EOF
cat > Client/Consumer.cs <<'EOF'
namespace Client; public class Consumer { public int Value => Library.Answer.Value; }
EOF

dotnet --version
csharp-ls --version
dotnet restore Smoke.slnx --configfile NuGet.Config
dotnet build Smoke.slnx --no-restore
