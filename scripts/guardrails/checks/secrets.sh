# secrets.sh — a secrets file must never be tracked by git.
#
# .dev.vars (backend-ts) and .env hold real credentials; their .example twins
# are the tracked templates. Once a secret is committed it is in the history
# forever, so this check is about the one moment it can still be cheap to fix.
#
# It reads `git ls-files`, so it needs a real repository — which is why the test
# suite builds one rather than committing a fixture .git (git refuses to track a
# nested repository and would leave it out of every clone).

gr_check_secrets() {
  mode=$1
  [ "$mode" = 'repo' ] || return 0
  git rev-parse --git-dir >/dev/null 2>&1 || return 0

  for target in $GR_SECRET_FILES; do
    if git ls-files --error-unmatch "$target" >/dev/null 2>&1; then
      gr_violation secrets "$target" \
        'tracked by git — it holds secrets' \
        "run: git rm --cached $target && echo $target >> .gitignore (and rotate anything already pushed)"
    fi
  done
}
