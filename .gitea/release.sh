#!/bin/sh
# Release auf Gitea über die REST-API (siehe workflows/release.yml), Gegenstück zu
# .github/release.sh. Die Dateien gehen direkt an einen Entwurf des Releases.
# Braucht curl und jq sowie GITEA_TOKEN, GITHUB_REPOSITORY und GITHUB_API_URL (setzt Gitea Actions,
# die API-Adresse endet dort auf /api/v1).
#
#   release.sh prepare <version> <sha> <true|false>   Entwurf anlegen, gibt die ID aus
#   release.sh upload <id> <datei>…                   Dateien an den Entwurf hängen
#   release.sh publish <id>                           SHA256SUMS zusammenfassen, veröffentlichen
set -eu
api="$GITHUB_API_URL/repos/$GITHUB_REPOSITORY"

call() {
  method=$1
  url=$2
  shift 2
  curl -sSf -X "$method" -H "Authorization: token $GITEA_TOKEN" -H "Accept: application/json" "$@" "$url"
}

# Ergebnisse der API immer erst in eine Variable: In einer Pipe ginge ein Fehler von curl
# verloren, weil nur der Exit-Code des letzten Befehls zählt
case $1 in
  prepare)
    version=$2 sha=$3 prerelease=$4
    # Entwürfe früherer, abgebrochener Läufe mit diesem Tag entfernen
    list=$(call GET "$api/releases?draft=true&limit=50")
    for old in $(echo "$list" | jq -r --arg tag "v$version" '.[] | select(.draft and .tag_name == $tag) | .id'); do
      call DELETE "$api/releases/$old" > /dev/null
      echo "Alten Entwurf $old entfernt" >&2
    done
    # Gitea legt den Tag erst beim Veröffentlichen an, auf dem Commit aus target_commitish
    body=$(jq -n --arg tag "v$version" --arg sha "$sha" --arg name "BosLook $version" --argjson pre "$prerelease" \
      '{tag_name: $tag, target_commitish: $sha, name: $name, draft: true, prerelease: $pre}')
    created=$(call POST "$api/releases" -H "Content-Type: application/json" --data "$body")
    id=$(echo "$created" | jq -r '.id // empty')
    if [ -z "$id" ]; then
      echo "Entwurf nicht angelegt: $created" >&2
      exit 1
    fi
    echo "$id"
    ;;
  upload)
    id=$2
    shift 2
    for file in "$@"; do
      name=$(basename "$file")
      call POST "$api/releases/$id/assets?name=$name" -F "attachment=@$file" > /dev/null
      echo "Hochgeladen: $name" >&2
    done
    ;;
  publish)
    id=$2
    assets=$(call GET "$api/releases/$id/assets")
    # Jeder Build-Job hat seine Prüfsummen als SHA256SUMS-<target> abgelegt
    : > SHA256SUMS
    for part in $(echo "$assets" | jq -r '.[] | select(.name | startswith("SHA256SUMS-")) | "\(.id)|\(.browser_download_url)"'); do
      curl -sSfL -H "Authorization: token $GITEA_TOKEN" "${part#*|}" >> SHA256SUMS
      call DELETE "$api/releases/$id/assets/${part%%|*}" > /dev/null
    done
    cat SHA256SUMS
    if [ ! -s SHA256SUMS ]; then
      echo "Keine Prüfsummen gefunden" >&2
      exit 1
    fi
    "$0" upload "$id" SHA256SUMS
    published=$(call PATCH "$api/releases/$id" -H "Content-Type: application/json" --data '{"draft": false}')
    echo "$published" | jq -r '"Veröffentlicht: \(.html_url)"'
    ;;
  *)
    echo "Unbekannter Befehl: $1" >&2
    exit 1
    ;;
esac
