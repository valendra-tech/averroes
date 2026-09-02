#!/usr/bin/env bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORKFLOW="${PROJECT_ROOT}/.github/workflows/macos-release.yml"
BUNDLE_SCRIPT="${PROJECT_ROOT}/scripts/bundle-macos.sh"

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

[[ -f "${WORKFLOW}" ]] || fail "workflow is missing: ${WORKFLOW}"
[[ -f "${BUNDLE_SCRIPT}" ]] || fail "bundle script is missing: ${BUNDLE_SCRIPT}"

if ! bash -n "${BUNDLE_SCRIPT}"; then
    fail "bundle script has invalid Bash syntax"
fi

if ! ruby -ryaml -e 'YAML.safe_load(File.read(ARGV.fetch(0)), aliases: true)' "${WORKFLOW}"; then
    fail "workflow has invalid YAML syntax"
fi

temporary_dir="$(mktemp -d "${PROJECT_ROOT}/tests/.verify-macos-release.XXXXXX")"
cleanup() {
    rm -rf "${temporary_dir}"
}
trap cleanup EXIT

# Extract every workflow run block through Psych before asking Bash to parse it.
ruby - "${WORKFLOW}" "${temporary_dir}" <<'RUBY'
require "yaml"

workflow_path, output_directory = ARGV

begin
  workflow = YAML.safe_load(File.read(workflow_path), aliases: true)
rescue Psych::Exception => error
  warn "FAIL: cannot parse workflow YAML: #{error.message}"
  exit 1
end

unless workflow.is_a?(Hash) && workflow["jobs"].is_a?(Hash)
  warn "FAIL: workflow must contain a jobs mapping"
  exit 1
end

run_count = 0
workflow.fetch("jobs").each_value do |job|
  next unless job.is_a?(Hash)

  Array(job["steps"]).each do |step|
    next unless step.is_a?(Hash) && step["run"].is_a?(String)

    run_count += 1
    # GitHub evaluates expressions before handing the script to Bash.
    script = step.fetch("run").gsub(/\$\{\{.*?\}\}/m, "GITHUB_EXPRESSION")
    File.write(File.join(output_directory, format("run-%03d.sh", run_count)), script)
  end
end

if run_count.zero?
  warn "FAIL: workflow does not contain embedded run scripts"
  exit 1
end
RUBY

for run_script in "${temporary_dir}"/*.sh; do
    if ! bash -n "${run_script}"; then
        fail "embedded workflow run script has invalid Bash syntax: $(basename "${run_script}")"
    fi
done

ruby - "${WORKFLOW}" "${BUNDLE_SCRIPT}" <<'RUBY'
require "yaml"

def fail(message)
  warn "FAIL: #{message}"
  exit 1
end

def assert!(condition, message)
  fail(message) unless condition
end

def job_steps(job)
  Array(job["steps"]).select { |step| step.is_a?(Hash) }
end

def run_text(job)
  job_steps(job)
    .map { |step| step["run"] if step["run"].is_a?(String) }
    .compact
    .join("\n")
end

def dependencies(job)
  case job["needs"]
  when String then [job.fetch("needs")]
  when Array then job.fetch("needs")
  else []
  end
end

def pinned_artifact_action?(step, action)
  step["uses"].is_a?(String) &&
    step.fetch("uses").match?(%r{\A#{Regexp.escape(action)}@[0-9a-f]{40}\z})
end

def artifact_action_steps(job, action)
  job_steps(job).select do |step|
    step["uses"].is_a?(String) && step.fetch("uses").start_with?("#{action}@")
  end
end

def action_steps(job)
  job_steps(job).select { |step| step["uses"].is_a?(String) }
end

def workflow_action_steps(jobs)
  jobs.values.flat_map { |job| job.is_a?(Hash) ? action_steps(job) : [] }
end

def executable_shell_lines(source)
  heredoc_terminator = nil
  lines = []

  source.each_line do |line|
    if heredoc_terminator
      heredoc_terminator = nil if line.chomp == heredoc_terminator
      next
    end

    heredoc = line.match(/<<-?\s*['"]?([A-Za-z_][A-Za-z0-9_]*)['"]?/)
    heredoc_terminator = heredoc[1] if heredoc
    next if line.match?(/^\s*#/)

    lines << line
  end

  lines
end

def keychain_configuration_mutation?(line)
  security_command = line.match(
    /^\s*(?:command\s+)?(?:\/usr\/bin\/)?security\s+([A-Za-z-]+)(?:\s+(.*))?$/
  )
  return false unless security_command

  subcommand, arguments = security_command.captures
  return true if subcommand == "default-keychain"
  return false unless subcommand == "list-keychains"

  arguments.to_s.match?(/(?:\A|[[:space:]])-s(?:[[:space:]]|\z)/)
end

def conditional_depth_at(lines, target_index)
  depth = 0

  lines.each_with_index do |line, index|
    break if index == target_index

    depth += 1 if line.match?(/^\s*if(?:[[:space:]]|\[)/)
    depth -= 1 if line.match?(/^\s*fi(?:[[:space:];]|\z)/)
  end

  depth
end

workflow_path, bundle_path = ARGV

begin
  workflow = YAML.safe_load(File.read(workflow_path), aliases: true)
rescue Psych::Exception => error
  fail("cannot parse workflow YAML: #{error.message}")
end

assert!(workflow.is_a?(Hash), "workflow must parse to a mapping")

workflow_source = File.read(workflow_path)
bundle = File.read(bundle_path)
assert!(bundle.include?("MINIMUM_MACOS_VERSION=\"13.0\""),
        "bundle script must target macOS 13.0")
assert!(bundle.include?('export MACOSX_DEPLOYMENT_TARGET="${MINIMUM_MACOS_VERSION}"'),
        "bundle script must export its minimum macOS version to Cargo")
assert!(bundle.match?(/<key>LSMinimumSystemVersion<\/key>\s*<string>\$\{MINIMUM_MACOS_VERSION\}<\/string>/m),
        "Info.plist must use the shared minimum macOS version")

assert!(bundle.match?(/normalize_architecture\(\)/),
        "bundle script must normalize architecture names")
assert!(bundle.match?(/x86_64\|amd64\).*x86_64/m),
        "bundle script must normalize x86_64 and amd64")
assert!(bundle.match?(/arm64\|aarch64\).*arm64/m),
        "bundle script must normalize arm64 and aarch64")
assert!(bundle.include?('${EXPECTED_ARCH+x}'),
        "bundle script must distinguish an omitted EXPECTED_ARCH from an empty one")
assert!(bundle.include?('EXPECTED_ARCH="${ARCH}"'),
        "bundle script must use the local architecture when EXPECTED_ARCH is omitted")
assert!(bundle.match?(/ARCH\}"\s*!=\s*"\$\{EXPECTED_ARCH\}/),
        "bundle script must reject an architecture mismatch")

assert!(bundle.match?(/for tool in cargo hdiutil plutil ditto vtool lipo;/),
        "bundle script must require vtool and lipo")
assert!(bundle.match?(/vtool\s+-show-build\s+"\$\{BINARY\}"/),
        "bundle script must inspect deployment targets with vtool")
assert!(bundle.match?(/platform.*MACOS/m),
        "bundle script must require a macOS vtool platform")
assert!(bundle.match?(/minos.*MINIMUM_MACOS_VERSION/m),
        "bundle script must require the configured vtool minimum OS")
assert!(bundle.match?(/lipo\s+-archs\s+"\$\{BINARY\}"/),
        "bundle script must inspect package architectures with lipo")

binary_check = bundle.index('if [[ ! -x "${BINARY}" ]]')
vtool_check = bundle.index('vtool -show-build "${BINARY}"')
lipo_check = bundle.index('lipo -archs "${BINARY}"')
bundle_creation = bundle.index('echo "Creating ${BUNDLE}..."')
assert!(binary_check && vtool_check && lipo_check && bundle_creation &&
          binary_check < vtool_check && vtool_check < lipo_check && lipo_check < bundle_creation,
        "binary compatibility checks must run after Cargo and before bundle creation")

normalized_bundle = bundle.gsub(/\\\s*\n/, " ")
bundle_shell_lines = executable_shell_lines(normalized_bundle)
keychain_mutations = bundle_shell_lines.select do |line|
  keychain_configuration_mutation?(line)
end
assert!(keychain_mutations.empty?,
        "bundle script must not change the user or runner keychain configuration")

hdiutil_verify_lines = bundle_shell_lines.each_index.select do |index|
  bundle_shell_lines[index].match?(/^\s*hdiutil\s+verify\s+"\$\{DMG\}"\s*(?:#.*)?$/)
end
assert!(!hdiutil_verify_lines.empty?, "bundle script must verify the final DMG")
assert!(hdiutil_verify_lines.any? { |index| conditional_depth_at(bundle_shell_lines, index).zero? },
        "final DMG verification must be outside conditional blocks so signed and unsigned DMGs are both verified")

signing_commands = normalized_bundle.lines.select do |line|
  line.match?(/\bcodesign\b/) && line.match?(/(?:\A|\s)(?:--sign|-s)(?:\s|\z)/)
end
assert!(!signing_commands.empty?, "bundle script must sign release artifacts")
signing_commands.each do |command|
  assert!(command.match?(/(?:\A|\s)--timestamp(?:\s|\z)/),
          "release signing must request a secure timestamp")
  assert!(!command.match?(/(?:\A|\s)--deep(?:\s|\z)/),
          "release signing must not use --deep")
  assert!(command.include?('"${codesign_keychain_args[@]+"${codesign_keychain_args[@]}"}"'),
          "release signing must expand an optional keychain safely with Bash 3.2 and set -u")
end
assert!(bundle.match?(/codesign_keychain_args=\(--keychain "\$\{CODESIGN_KEYCHAIN\}"\)/),
        "bundle script must pass CODESIGN_KEYCHAIN to codesign")
assert!(normalized_bundle.match?(/\bcodesign\s+--force\s+--options\s+runtime\s+--timestamp\s+--sign\s+"\$\{CODESIGN_IDENTITY\}".*"\$\{BUNDLE\}"/),
        "app signing must enable the hardened runtime and timestamp")
assert!(normalized_bundle.match?(/\bcodesign\s+--force\s+--timestamp\s+--sign\s+"\$\{CODESIGN_IDENTITY\}".*"\$\{DMG\}"/),
        "DMG signing must request a timestamp")

verification_commands = normalized_bundle.lines.select do |line|
  line.match?(/\bcodesign\b/) && line.match?(/(?:\A|\s)--verify(?:\s|\z)/)
end
assert!(verification_commands.any? { |command| command.match?(/--verify\s+--deep\s+--strict/) },
        "bundle script must strictly verify the app signature")
assert!(verification_commands.all? { |command| command.include?('"${codesign_keychain_args[@]+"${codesign_keychain_args[@]}"}"') },
        "signature verification must expand an optional keychain safely with Bash 3.2 and set -u")

dmg_creation = normalized_bundle.index('hdiutil create')
dmg_signing = normalized_bundle.index('codesign --force --timestamp --sign "${CODESIGN_IDENTITY}"')
dmg_verification = normalized_bundle.index('hdiutil verify "${DMG}"')
artifact_output = normalized_bundle.index('echo "App bundle: ${BUNDLE}"')
assert!(dmg_creation && dmg_signing && dmg_verification && artifact_output &&
          dmg_creation < dmg_signing && dmg_signing < dmg_verification && dmg_verification < artifact_output,
        "bundle script must verify the final DMG after signing and before output")

# Psych treats the YAML 1.1 key `on` as boolean true on some Ruby versions.
triggers = workflow.key?("on") ? workflow["on"] : workflow[true]
assert!(triggers.is_a?(Hash), "workflow must define a push tag trigger")
assert!(triggers.keys == ["push"], "workflow must trigger only from pushes")
push_trigger = triggers["push"]
assert!(push_trigger.is_a?(Hash), "workflow must define a push tag trigger")
tag_patterns = Array(push_trigger["tags"])
assert!(tag_patterns == ["v*"], "workflow push trigger must match exactly v* tags")
assert!(workflow_source.match?(/^\s*-\s*['"]v\*['"]\s*$/),
         "workflow v* tag pattern must be quoted")

permissions = workflow["permissions"]
assert!(permissions.is_a?(Hash) && permissions["contents"] == "read",
         "workflow must set global contents permission to read")

if workflow.key?("concurrency")
  concurrency = workflow.fetch("concurrency")
  assert!(concurrency.is_a?(Hash) && concurrency["group"].to_s.include?("github.ref_name"),
          "workflow concurrency must be keyed by the tag name")
  assert!(concurrency["cancel-in-progress"] == false,
          "workflow concurrency must not cancel an in-progress release")
end

jobs = workflow["jobs"]
assert!(jobs.is_a?(Hash), "workflow must contain a jobs mapping")
%w[validate build publish].each do |name|
  assert!(jobs[name].is_a?(Hash), "workflow must define a #{name} job")
end

validate = jobs.fetch("validate")
build = jobs.fetch("build")
publish = jobs.fetch("publish")
assert!(dependencies(build).include?("validate"), "build job must depend on validate")
assert!(dependencies(publish).include?("build"), "publish job must depend on build")
assert!(dependencies(publish).include?("validate"), "publish job must receive validated release outputs")

validate_permissions = validate["permissions"]
assert!(validate_permissions.is_a?(Hash) && validate_permissions["contents"] == "read",
         "validate job must remain read-only")
assert!(validate["runs-on"].to_s == "ubuntu-latest",
         "validate job must run on a safe runner")

validate_checkout = action_steps(validate).find do |step|
  step.fetch("uses", "").start_with?("actions/checkout@")
end
assert!(!validate_checkout.nil?, "validate job must check out the pushed tag")
assert!(validate_checkout.dig("with", "ref") == "${{ github.ref }}",
         "validate checkout must use github.ref")
assert!(validate_checkout.dig("with", "fetch-depth").to_s == "0",
         "validate checkout must fetch full history")
assert!(validate_checkout.dig("with", "persist-credentials") == false,
         "validate checkout must not persist credentials")

validate_outputs = validate["outputs"]
assert!(validate_outputs.is_a?(Hash) && validate_outputs.key?("tag") && validate_outputs.key?("version"),
         "validate job must expose tag and version outputs")
validate_runs = run_text(validate).gsub(/\\\s*\n/, " ")
bundle_semver_regex = bundle[/SEMVER_REGEX='([^']+)'/, 1]
tag_semver_regex = validate_runs[/SEMVER_REGEX='([^']+)'/, 1]
assert!(!bundle_semver_regex.nil? && tag_semver_regex == bundle_semver_regex.sub(/\A\^/, "^v") &&
        validate_runs.match?(/GITHUB_REF_NAME/) &&
        validate_runs.match?(/=~\s+\$\{SEMVER_REGEX\}/),
        "validate job must require a v-prefixed full SemVer tag")
assert!(validate_runs.match?(/git\s+rev-parse\s+"\$\{GITHUB_REF\}\^\{commit\}"/) &&
        validate_runs.match?(/git\s+fetch\s+--no-tags\s+origin\s+main:refs\/remotes\/origin\/main/) &&
        validate_runs.match?(/git\s+merge-base\s+--is-ancestor\s+"\$\{TAG_COMMIT\}"\s+refs\/remotes\/origin\/main/),
        "validate job must reject tags whose commits are not reachable from main")
assert!(validate_runs.match?(/GITHUB_OUTPUT/) && validate_runs.match?(/GITHUB_REF_NAME#v/),
        "validate job must safely write validated tag and version outputs")

publish_permissions = publish["permissions"]
assert!(publish_permissions.is_a?(Hash) && publish_permissions["contents"] == "write",
         "publish job must grant contents write permission")

build_permissions = build["permissions"]
assert!(build_permissions.is_a?(Hash) && build_permissions["contents"] == "read",
         "build job must remain read-only")
assert!(build["timeout-minutes"].to_s == "45", "build job must have a 45 minute timeout")
assert!(build.dig("strategy", "fail-fast") == false, "build matrix must not fail fast")

matrix_entries = build.dig("strategy", "matrix", "include")
assert!(matrix_entries.is_a?(Array), "build job must define an architecture matrix")
architectures = matrix_entries.map do |entry|
  entry["arch"] if entry.is_a?(Hash) && entry["arch"].is_a?(String)
end.compact
assert!(architectures.sort == %w[arm64 x86_64],
         "build matrix must define exactly arm64 and x86_64 architectures")
assert!(matrix_entries.map { |entry| entry.slice("runner", "arch") }.sort_by { |entry| entry.fetch("arch") } == [
          { "runner" => "macos-15", "arch" => "arm64" },
          { "runner" => "macos-15-intel", "arch" => "x86_64" }
        ], "build matrix must use the required macOS runners")

expected_actions = {
  "actions/checkout" => "11bd71901bbe5b1630ceea73d27597364c9af683",
  "dtolnay/rust-toolchain" => "4360b52568e2003a75bf9bc1d59f33a8e3fc893c",
  "Swatinem/rust-cache" => "6323deb102c322ba6fcbdcafc7e3dddab59af2b6",
  "actions/upload-artifact" => "ea165f8d65b6e75b540449e92b4886f43607fa02",
  "actions/download-artifact" => "d3f86a106a0bac45b974a628896c90dbdf5c8093"
}
all_action_steps = workflow_action_steps(jobs)
assert!(all_action_steps.all? { |step| step.fetch("uses").match?(/@[0-9a-f]{40}\z/) },
         "every external action must be pinned to a full commit SHA")
expected_actions.each do |action, sha|
  matching_steps = all_action_steps.select { |step| step.fetch("uses").start_with?("#{action}@") }
  assert!(!matching_steps.empty? && matching_steps.all? { |step| step.fetch("uses") == "#{action}@#{sha}" },
          "workflow must use the approved pinned #{action} action")
end

upload_steps = artifact_action_steps(build, "actions/upload-artifact")
download_steps = artifact_action_steps(publish, "actions/download-artifact")
assert!(!upload_steps.empty? && upload_steps.all? { |step| pinned_artifact_action?(step, "actions/upload-artifact") },
        "build job must use only commit-pinned actions/upload-artifact actions")
assert!(!download_steps.empty? && download_steps.all? { |step| pinned_artifact_action?(step, "actions/download-artifact") },
         "publish job must use only commit-pinned actions/download-artifact actions")

validated_uploads = upload_steps.select do |step|
  step.dig("with", "name") == "validated-dmg-${{ matrix.arch }}"
end
assert!(validated_uploads.length == 1, "build job must upload one validated DMG per architecture")
validated_upload = validated_uploads.fetch(0)
assert!(validated_upload.dig("with", "path") == "${{ steps.bundle.outputs.dmg }}",
         "validated artifact must contain the final DMG output")
assert!(validated_upload.dig("with", "retention-days").to_s == "7" &&
        validated_upload.dig("with", "if-no-files-found") == "error",
        "validated DMG artifacts must have seven-day retention and fail when missing")

diagnostic_uploads = upload_steps.select do |step|
  step.dig("with", "name") == "notarization-diagnostics-${{ matrix.arch }}"
end
assert!(diagnostic_uploads.length == 1 && diagnostic_uploads.fetch(0)["if"].to_s.include?("failure()"),
        "build job must upload notarization diagnostics only on failure")
assert!(diagnostic_uploads.fetch(0).dig("with", "retention-days").to_s == "7" &&
        diagnostic_uploads.fetch(0).dig("with", "if-no-files-found") == "ignore",
        "notarization diagnostics must use seven-day retention and tolerate absent files")

download_names = download_steps.map { |step| step.dig("with", "name") }.sort
assert!(download_names == %w[validated-dmg-arm64 validated-dmg-x86_64],
        "publish job must download only the private validated DMG artifacts")
assert!(!run_text(build).match?(/\bgh\s+release\b/),
        "build job must not publish release assets directly")

secret_names = workflow_source.scan(/\$\{\{\s*secrets\.([A-Za-z0-9_]+)\s*\}\}/).flatten.uniq
expected_secret_names = %w[
  APPLE_CERTIFICATE_BASE64
  APPLE_CERTIFICATE_PASSWORD
  APPLE_CODESIGN_IDENTITY
  APPLE_API_KEY_BASE64
  APPLE_API_KEY_ID
  APPLE_API_ISSUER_ID
]
assert!(secret_names.sort == expected_secret_names.sort,
        "workflow must use only the required certificate and API-key secrets")
%w[APPLE_ID APPLE_TEAM_ID APPLE_APP_SPECIFIC_PASSWORD].each do |legacy_secret|
  assert!(!secret_names.include?(legacy_secret),
          "workflow must not use the legacy #{legacy_secret} secret")
  assert!(!workflow_source.match?(/\b#{Regexp.escape(legacy_secret)}\b/),
          "workflow must not reference the legacy #{legacy_secret} credential")
end

assert!(workflow_source.match?(/\bxcrun\s+notarytool\s+(?:store-credentials|submit)\b.*?--key\s+"?\$\{?API_KEY_PATH\}?"?.*?--key-id\s+"?\$\{?APPLE_API_KEY_ID\}?"?.*?--issuer\s+"?\$\{?APPLE_API_ISSUER_ID\}?"?/m),
        "notarization must authenticate with the temporary App Store Connect API key")

build_runs = run_text(build).gsub(/\\\s*\n/, " ")
assert!(build_runs.match?(/\bspctl\s+--assess\s+--type\s+open\s+--context\s+context:primary-signature\b/),
         "DMG Gatekeeper assessment must use context:primary-signature")

build_shell_lines = executable_shell_lines(build_runs)
workflow_keychain_mutations = build_shell_lines.select do |line|
  keychain_configuration_mutation?(line)
end
assert!(build_runs.match?(/security\s+list-keychains\s+-d\s+user\s*>\s+"\$\{ORIGINAL_KEYCHAINS_PATH\}"/),
        "workflow must save the original keychain search list")
assert!(build_runs.match?(/security\s+list-keychains\s+-d\s+user\s+-s\s+"\$\{KEYCHAIN_PATH\}"/),
        "workflow must prepend the signing keychain to the search list")
assert!(!workflow_keychain_mutations.empty?,
        "workflow must expose the temporary signing keychain while signing")
assert!(build_runs.match?(/security\s+find-identity\s+-v\s+-p\s+codesigning\s+"\$\{KEYCHAIN_PATH\}"/) &&
        build_runs.match?(/APPLE_CODESIGN_IDENTITY/),
        "workflow must verify the configured signing identity in the temporary keychain")

staple_index = build_runs.index("xcrun stapler staple")
stapler_validate_index = build_runs.index("xcrun stapler validate")
post_staple_hdiutil_index = staple_index && build_runs.index("hdiutil verify", staple_index)
dmg_assessment_index = build_runs.index("spctl --assess --type open")
assert!(staple_index && stapler_validate_index && post_staple_hdiutil_index && dmg_assessment_index &&
        staple_index < stapler_validate_index && stapler_validate_index < post_staple_hdiutil_index &&
        post_staple_hdiutil_index < dmg_assessment_index,
        "workflow must verify the stapled final DMG before Gatekeeper assessment")

cleanup_steps = job_steps(build).select do |step|
  step["if"].to_s.include?("always()") && step["run"].is_a?(String) &&
    step.fetch("run").match?(/security\s+delete-keychain/)
end
assert!(!cleanup_steps.empty?, "workflow must always clean up the temporary signing keychain")
cleanup_text = cleanup_steps
  .map { |step| step["run"] if step.is_a?(Hash) }
  .compact
  .join("\n")
assert!(cleanup_text.match?(/security\s+list-keychains\s+-d\s+user\s+-s/),
        "workflow must restore the original keychain search list during cleanup")
assert!(cleanup_text.match?(/\brm\s+-[^\n]*f[^\n]*CERTIFICATE_PATH/),
        "credential cleanup must remove the temporary certificate")
assert!(cleanup_text.match?(/\brm\s+-[^\n]*f[^\n]*API_KEY_PATH/),
        "credential cleanup must remove the temporary API key")

publish_runs = run_text(publish).gsub(/\\\s*\n/, " ")
draft_creation = publish_runs.match(/\bgh\s+release\s+create\b[^\n]*/m)
upload = publish_runs.match(/\bgh\s+release\s+upload\b/m)
publication = publish_runs.match(/\bgh\s+release\s+edit\b[^\n]*/m)
assert!(!draft_creation.nil? && draft_creation.to_s.match?(/\bgh\s+release\s+create\s+"\$RELEASE_TAG"\s+--draft\s+--generate-notes\s+--verify-tag/),
         "publish job must create a verified draft release before publishing")
assert!(!upload.nil?, "publish job must upload artifacts to the draft release")
assert!(!publication.nil? && publication.to_s.match?(/--draft=false\b/),
         "publish job must publish the draft release")
assert!(draft_creation && upload && publication &&
           draft_creation.begin(0) < upload.begin(0) && upload.begin(0) < publication.begin(0),
         "publish job must create, upload to, then publish the release")
assert!(publish_runs.match?(/\bgh\s+release\s+view\s+"\$RELEASE_TAG"\s+--json\s+isDraft/) &&
        publish_runs.match?(/\bgh\s+release\s+view\s+"\$RELEASE_TAG"\s+--json\s+assets/) &&
        publish_runs.match?(/\bshasum\s+-a\s+256\b/) &&
        publish_runs.match?(/SHA256SUMS\.txt/) &&
        publish_runs.match?(/\bgh\s+release\s+upload\s+"\$RELEASE_TAG".*--clobber/m),
        "publish job must validate draft assets and upload ordered DMGs with checksums")
RUBY
