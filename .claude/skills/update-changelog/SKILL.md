---
name: update-changelog
description: Update the Unreleased section of CHANGELOG.md from the commits since the last version, listing new features and folding small changes into one "Bug fixes" line. Use whenever the user asks to update, write or add to the changelog, or to note what changed for the next release.
---

# Update the changelog

The changelog tells people what they can do now that they couldn't before, and
nothing more. New features get an entry each. Everything small is summed up as
one line, `Bug fixes.`

## 1. Find what changed

Versions are `0.1.<commits>`, so the newest version in the changelog says which
commit it stopped at. Everything after that commit is new.

```sh
last=$(grep -m1 -oE '^## [0-9]+\.[0-9]+\.[0-9]+' CHANGELOG.md | cut -d' ' -f2)
base=$(git rev-list --reverse HEAD | sed -n "${last##*.}p")
git log --reverse --format='%h %s%n%b' "$base"..HEAD
```

A subject line can undersell or oversell a commit. When you can't tell what
one did for the user, read its diff.

## 2. Sort each commit

| Kind of commit | What to write |
|---|---|
| **New feature**: the user can do or see something new, or something they rely on now works differently | One entry |
| **Small change to the app**: a fix, a tweak, a tidier layout, a speed-up | Counts towards `Bug fixes.` |
| **No effect on the app**: docs, README, CI, tests, build, skills, changelog, refactors | Nothing |

Keep the third row out because `Bug fixes.` has to stay true: a release that only
touched the README fixed nothing.

When unsure between the first two rows, ask whether a user would want to be told
about it by name. If not, it's a small change.

## 3. Write it

The section goes above the newest version and looks like this:

```markdown
## Unreleased

- Bookmarks: right-click a line or press Ctrl+Shift+B to mark it; marked lines stay tinted as they scroll.
- `fastcnsl -V` and `--version` print the version.
- Bug fixes.
```

- A plain list with no `###` subheadings, features first.
- `Bug fixes.` comes last, once, however many small changes there were. Leave it
  out if there were none.
- Write for someone using the app: what it does and how to reach it (the menu,
  the key, the setting), not how it was built.
- Several commits that built one feature are one entry describing where it ended
  up. If a feature was added and then reverted, it gets no entry.
- If a default changed, say so first. It's what people will notice.

If `## Unreleased` already exists, keep what's there and add only what's missing,
so running this twice changes nothing. Don't touch the sections of released
versions.

## 4. Finish

Show the user the section, and commit `CHANGELOG.md`. Don't push; that is what
`/push-changelog` is for, and it needs the section this skill writes. If no
commit needs an entry, leave the file alone and say so.
