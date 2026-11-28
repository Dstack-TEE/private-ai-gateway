#!/usr/bin/env bash
set -euo pipefail

manifests="$(realpath "$1")"
owner="$2"
upstream=microsoft/winget-pkgs
repository="${upstream#*/}"
fork="$owner/$repository"
[[ "$owner" =~ ^[A-Za-z0-9]([A-Za-z0-9-]{0,37}[A-Za-z0-9])?$ ]]
expected=$'Organization\t'"$upstream"
if [[ "$(gh api "repos/$fork" --jq '[.owner.type, .parent.full_name] | @tsv')" != "$expected" ]]; then
  echo "$fork must be an organization-owned fork of $upstream" >&2
  exit 1
fi

scratch="$(mktemp -d)"
trap 'rm -rf -- "$scratch"' EXIT
shopt -s globstar nullglob
installers=("$manifests"/manifests/**/*.installer.yaml)
if (( ${#installers[@]} == 0 )); then
  echo "No WinGet installer manifests in $manifests" >&2
  exit 1
fi

base="$(gh api "repos/$upstream" --jq .default_branch)"
git clone --quiet --depth 1 --filter=blob:none --no-checkout --single-branch --branch "$base" \
  "https://github.com/$upstream.git" "$scratch/checkout"
git_command=(git -C "$scratch/checkout" -c credential.helper= -c 'credential.helper=!gh auth git-credential')
"${git_command[@]}" remote add fork "https://github.com/$fork.git"
upstream_tip="$("${git_command[@]}" rev-parse "origin/$base")"
read -r author author_id < <(gh api user --jq '[.login, .id] | @tsv')
author_email="$author_id+$author@users.noreply.github.com"
fork_synced=false

for installer in "${installers[@]}"; do
  directory="$(dirname -- "$installer")"
  relative="${directory#"$manifests"/}"
  id="$(basename -- "$installer" .installer.yaml)"
  version="$(basename -- "$directory")"
  [[ "$id" =~ ^[A-Za-z0-9.]+$ && "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]
  branch="$id-$version"
  ref="refs/heads/$branch"
  if "${git_command[@]}" cat-file -e "$upstream_tip:$relative" 2>/dev/null; then
    echo "$id $version is already in $upstream"
    continue
  fi
  existing="$(gh api --method GET "repos/$upstream/pulls" -f "head=$owner:$branch" -f state=open --jq '.[0].html_url // empty')"
  branch_tip="$(gh api "repos/$fork/git/matching-refs/heads/$branch" \
    --jq ".[] | select(.ref == \"$ref\") | .object.sha")"
  if [[ -z "$branch_tip" ]]; then
    if [[ "$fork_synced" == false ]]; then
      fork_tip="$(gh api "repos/$fork/git/ref/heads/$base" --jq .object.sha)"
      if [[ "$fork_tip" != "$upstream_tip" ]]; then
        gh api --method POST "repos/$fork/merge-upstream" -f "branch=$base" --jq .message
      fi
      fork_synced=true
    fi
    # GitHub seeds the branch server-side. A shallow, blobless clone then
    # pushes only the new commit and objects for this package version.
    gh api --method POST "repos/$fork/git/refs" -f "ref=$ref" -f "sha=$upstream_tip" --silent
    branch_tip="$upstream_tip"
  elif [[ "$branch_tip" != "$upstream_tip" ]]; then
    "${git_command[@]}" fetch --quiet --depth 1 --filter=blob:none fork "$ref"
    branch_tip="$("${git_command[@]}" rev-parse FETCH_HEAD)"
  fi

  write_manifests=false
  legacy_files=()
  if "${git_command[@]}" cat-file -e "$branch_tip:$relative" 2>/dev/null; then
    for manifest in "$directory"/*.yaml; do
      filename="$(basename -- "$manifest")"
      expected_blob="$("${git_command[@]}" hash-object --no-filters "$manifest")"
      actual_blob="$("${git_command[@]}" rev-parse --quiet --verify "$branch_tip:$relative/$filename" || true)"
      legacy_blob=
      if [[ "$filename" == "$id.locale."*.yaml ]]; then
        legacy="$id.${filename#"$id.locale."}"
        legacy_blob="$("${git_command[@]}" rev-parse --quiet --verify "$branch_tip:$relative/$legacy" || true)"
        if [[ -n "$legacy_blob" ]]; then
          if [[ "$legacy_blob" != "$expected_blob" ]]; then
            echo "$fork:$branch has different content in $relative/$legacy" >&2
            exit 1
          fi
          legacy_files+=("$relative/$legacy")
        fi
      fi
      if [[ "$actual_blob" != "$expected_blob" && ( -n "$actual_blob" || -z "$legacy_blob" ) ]]; then
        echo "$fork:$branch has a missing or different manifest at $relative/$filename" >&2
        exit 1
      fi
    done
  else
    if [[ "$branch_tip" != "$upstream_tip" ]]; then
      # Reuse a branch seeded before upstream advanced, only if it still
      # points at an upstream commit.
      status="$(gh api "repos/$upstream/compare/$branch_tip...$upstream_tip" --jq .status)"
      [[ "$status" == ahead || "$status" == identical ]]
    fi
    write_manifests=true
  fi
  message="Publish $id version $version"
  if (( ${#legacy_files[@]} > 0 )); then
    # The old branch uses the same blobs. Seed the sparse checkout from the
    # local manifests so the blobless clone does not fetch them.
    "${git_command[@]}" hash-object -w --no-filters "$directory"/*.yaml >/dev/null
    message="Fix locale manifest filename for $id version $version"
    write_manifests=true
  fi
  if [[ "$write_manifests" == true ]]; then
    "${git_command[@]}" sparse-checkout set --no-cone "/$relative/"
    "${git_command[@]}" switch --quiet --detach "$branch_tip"
    if (( ${#legacy_files[@]} > 0 )); then
      "${git_command[@]}" rm --quiet -- "${legacy_files[@]}"
    fi
    mkdir -p "$scratch/checkout/$relative"
    cp -- "$directory"/*.yaml "$scratch/checkout/$relative/"
    "${git_command[@]}" add -- "$relative"
    "${git_command[@]}" -c "user.name=$author" -c "user.email=$author_email" \
      commit --quiet -m "$message"
    "${git_command[@]}" push --quiet fork "HEAD:$ref"
  fi
  if [[ -n "$existing" ]]; then
    echo "$id $version already has an open pull request: $existing"
    continue
  fi

  prior_versions="$("${git_command[@]}" ls-tree --name-only "$upstream_tip:${relative%/*}" 2>/dev/null || true)"
  title="New package: $id version $version"
  if [[ "$prior_versions" =~ (^|$'\n')[0-9] ]]; then
    title="Update: $id to $version"
  fi
  cat > "$scratch/pr-body.md" <<BODY
## 📖 Description

Publish $id version $version using the upstream GitHub release assets.

## ✅ Checklist

- [ ] Signed the [Contributor License Agreement](https://cla.opensource.microsoft.com)
- [ ] Linked to an issue (if applicable)

## 📦 Manifest Checklist

- [ ] Checked for duplicate [open pull requests](https://github.com/microsoft/winget-pkgs/pulls)
- [x] This PR only modifies one (1) manifest
- [ ] Validated locally with \`winget validate --manifest <path>\`
- [ ] Tested locally with \`winget install --manifest <path>\`
- [ ] Manifests conform to the [current repository schema](https://github.com/microsoft/winget-pkgs/tree/master/doc/manifest/schema)
BODY
  jq -n --arg head "$owner:$branch" --arg head_repo "$repository" --arg base "$base" \
    --arg title "$title" --rawfile body "$scratch/pr-body.md" \
    '{head: $head, head_repo: $head_repo, base: $base, title: $title, body: $body}' |
    gh api --method POST "repos/$upstream/pulls" --input - --jq .html_url
done
