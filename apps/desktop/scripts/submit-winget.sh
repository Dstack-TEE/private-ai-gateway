#!/usr/bin/env bash
set -euo pipefail

manifests="$(realpath "$1")"
owner="$2"
upstream=microsoft/winget-pkgs
repository="${upstream#*/}"
fork="$owner/$repository"
[[ "$owner" =~ ^[A-Za-z0-9]([A-Za-z0-9-]{0,37}[A-Za-z0-9])?$ ]]
expected=$'Organization\tmicrosoft/winget-pkgs'
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
  "https://github.com/$upstream.git" "$scratch"
git -C "$scratch" remote add fork "https://github.com/$fork.git"
git_command=(git -C "$scratch" -c credential.helper= -c 'credential.helper=!gh auth git-credential')
for installer in "${installers[@]}"; do
  directory="$(dirname -- "$installer")"
  relative="${directory#"$manifests"/}"
  id="$(basename -- "$installer" .installer.yaml)"
  version="$(basename -- "$directory")"
  [[ "$id" =~ ^[A-Za-z0-9.]+$ && "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]
  branch="$id-$version"
  if git -C "$scratch" cat-file -e "origin/$base:$relative" 2>/dev/null; then
    echo "$id $version is already in $upstream"
    continue
  fi
  existing="$(gh api --method GET "repos/$upstream/pulls" -f "head=$owner:$branch" -f state=all --jq '.[0].html_url // empty')"
  if [[ -n "$existing" ]]; then
    echo "$id $version already has a pull request: $existing"
    continue
  fi

  git -C "$scratch" sparse-checkout set --no-cone "/$relative/"
  git -C "$scratch" switch --quiet --detach "origin/$base"
  mkdir -p "$scratch/$relative"
  cp -- "$directory"/*.yaml "$scratch/$relative/"
  if "${git_command[@]}" ls-remote --exit-code --heads fork "$branch" >/dev/null; then
    # A previous run may have pushed the branch before it opened the PR.
    "${git_command[@]}" fetch --quiet --depth 1 fork "refs/heads/$branch"
    for manifest in "$directory"/*.yaml; do
      filename="$(basename -- "$manifest")"
      git -C "$scratch" show "FETCH_HEAD:$relative/$filename" | cmp - "$manifest"
    done
  else
    status=$?
    if (( status != 2 )); then exit "$status"; fi
    git -C "$scratch" add -- "$relative"
    git -C "$scratch" -c user.name='github-actions[bot]' -c user.email='41898282+github-actions[bot]@users.noreply.github.com' \
      commit --quiet -m "New version: $id version $version"
    "${git_command[@]}" push --quiet fork "HEAD:refs/heads/$branch"
  fi
  gh api --method POST "repos/$upstream/pulls" \
    -f "head=$owner:$branch" -f "head_repo=$repository" -f "base=$base" \
    -f "title=New version: $id version $version" --jq .html_url
done
