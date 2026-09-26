#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
sdk=${1:-"$root/.claude/tmp/issue79-dotnet/dotnet"}
source_dir=${2:-/tmp/actions-runner}
output=${3:-"$root/crates/expressions/src/tests/expression_reference.json"}
provenance=${4:-"$root/crates/expressions/src/tests/expression_reference.provenance.json"}
build_dir="$root/.claude/tmp/expression-reference-oracle"
inputs="$root/crates/expressions/src/tests/expression_inputs.json"

if [[ ! -x "$sdk" ]]; then
  echo "dotnet SDK executable is required: $sdk" >&2
  exit 1
fi
expected_sha=cab9d1c3901e45c7705889c4f88284fdd93f4ae5
if [[ ! -d "$source_dir/.git" ]] || [[ $(git -C "$source_dir" rev-parse HEAD) != "$expected_sha" ]]; then
  echo "reference source must be the pinned checkout $expected_sha" >&2
  exit 1
fi
if [[ -n $(git -C "$source_dir" status --porcelain --untracked-files=all -- src) ]]; then
  echo "reference source must be clean under src/" >&2
  exit 1
fi
python3 - "$inputs" <<'PY'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as source:
    expressions = json.load(source)
if not isinstance(expressions, list) or len(expressions) != 1402:
    raise SystemExit("expression input corpus must contain exactly 1402 expressions")
PY

export DOTNET_CLI_HOME="$root/.claude/tmp/expression-reference-dotnet-home"
export NUGET_PACKAGES="$root/.claude/tmp/expression-reference-nuget"
export TMPDIR="$root/.claude/tmp/expression-reference-tmp"
mkdir -p "$DOTNET_CLI_HOME" "$NUGET_PACKAGES" "$TMPDIR" "$build_dir"
rsync -a --delete "$source_dir/src/" "$build_dir/src/"
mkdir -p "$build_dir/probe"

cat > "$build_dir/probe/ExpressionReference.csproj" <<'EOF'
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net8.0</TargetFramework>
    <OutputType>Exe</OutputType>
    <AssemblyName>Test</AssemblyName>
    <ImplicitUsings>enable</ImplicitUsings>
    <Nullable>enable</Nullable>
  </PropertyGroup>
  <ItemGroup>
    <ProjectReference Include="../src/Sdk/Sdk.csproj" />
  </ItemGroup>
</Project>
EOF

cat > "$build_dir/probe/Program.cs" <<'EOF'
using System.Text.Json;
using GitHub.DistributedTask.Expressions2;
using GitHub.DistributedTask.Expressions2.Sdk;
using GitHub.DistributedTask.Pipelines.ContextData;

sealed class ContextValueNode : NamedValue
{
    protected override object? EvaluateCore(EvaluationContext context, out ResultMemory? resultMemory)
    {
        resultMemory = null;
        return ((Dictionary<string, object>)context.State!)[Name];
    }
}

sealed record OutputRow(string expression, string kind, string value);

static class Program
{
    static int Main(string[] args)
    {
        if (args.Length != 2) throw new ArgumentException("expected input and output paths");
        var expressions = JsonSerializer.Deserialize<string[]>(File.ReadAllText(args[0]))
            ?? throw new InvalidOperationException("input must be a JSON array");
        var state = new Dictionary<string, object> { ["github"] = GitHub(), ["vars"] = new DictionaryContextData() };
        var namedValues = new INamedValueInfo[] { new NamedValueInfo<ContextValueNode>("github"), new NamedValueInfo<ContextValueNode>("vars") };
        var parser = new ExpressionParser();
        var rows = new List<OutputRow>(expressions.Length);
        foreach (var expression in expressions)
        {
            try
            {
                var result = Evaluate(parser, namedValues, state, expression);
                var value = result.Kind is ValueKind.Array or ValueKind.Object
                    ? Evaluate(parser, namedValues, state, $"toJSON({expression})").ConvertToString()
                    : result.ConvertToString();
                rows.Add(new OutputRow(expression, result.Kind.ToString().ToLowerInvariant(), value));
            }
            catch (Exception exception)
            {
                rows.Add(new OutputRow(expression, "error", exception.Message));
            }
        }
        File.WriteAllText(args[1], JsonSerializer.Serialize(rows, new JsonSerializerOptions { WriteIndented = true }) + "\n");
        return 0;
    }

    static EvaluationResult Evaluate(ExpressionParser parser, INamedValueInfo[] namedValues, Dictionary<string, object> state, string expression) =>
        parser.CreateTree(expression, null, namedValues, null).Evaluate(null, null, state, null);

    static DictionaryContextData GitHub()
    {
        var github = new DictionaryContextData();
        github.Add("repository", new StringContextData("Falconiere/toolu-ghrunner"));
        var array = new ArrayContextData();
        array.Add(new NumberContextData(1));
        array.Add(new NumberContextData(2));
        github.Add("array", array);
        var obj = new DictionaryContextData();
        obj.Add("z", new NumberContextData(1));
        obj.Add("a", new NumberContextData(2));
        github.Add("object", obj);
        return github;
    }
}
EOF

"$sdk" run --project "$build_dir/probe/ExpressionReference.csproj" -- "$inputs" "$output"
python3 - "$inputs" "$source_dir" "$sdk" "$provenance" "$output" <<'PY'
import hashlib
import json
import subprocess
import sys

inputs, source, sdk, output, reference = sys.argv[1:]
def text(args):
    return subprocess.check_output(args, text=True).strip()
runtime = text([sdk, "--list-runtimes"]).splitlines()
runtime = next(line for line in runtime if line.startswith("Microsoft.NETCore.App ")).split()[1]
sdk_version = text([sdk, "--version"])
source_tar = subprocess.check_output(["git", "-C", source, "archive", "--format=tar", "HEAD", "src/Sdk"])
metadata = {
    "upstream_sha": text(["git", "-C", source, "rev-parse", "HEAD"]),
    "source_sdk_sha256": hashlib.sha256(source_tar).hexdigest(),
    "input_sha256": hashlib.sha256(open(inputs, "rb").read()).hexdigest(),
    "reference_sha256": hashlib.sha256(open(reference, "rb").read()).hexdigest(),
    "dotnet_sdk_version": sdk_version,
    "dotnet_runtime_version": runtime,
}
with open(output, "w", encoding="utf-8") as target:
    json.dump(metadata, target, indent=2, sort_keys=True)
    target.write("\n")
PY
echo "reference SHA: $(git -C "$source_dir" rev-parse HEAD)" >&2
