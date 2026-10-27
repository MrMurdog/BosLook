#!/bin/sh
# Release auf GitHub über die REST-API (siehe workflows/release.yml). Die Dateien gehen direkt an
# einen Entwurf des Releases statt über Artefakte, die gegen das Speicherlimit von Actions zählen.
# Braucht curl und jq sowie GH_TOKEN, GITHUB_REPOSITORY und GITHUB_API_URL (setzt Actions).
#
#   release.sh prepare <version> <sha> <true|false>   Entwurf anlegen, gibt die ID aus
#   release.sh upload <id> <datei>…                   Dateien an den Entwurf hängen
#   release.sh publish <id>                           SHA256SUMS zusammenfassen, veröffentlichen
set -eu

api="${GITHUB_API_URL:-https://api.github.com}/repos/$GITHUB_REPOSITORY"

call() {
  method=$1
  url=$2
  shift 2

  # Wichtig: Fehlerantwort inkl. Body ausgeben, damit die Ursache sichtbar wird
  curl --fail-with-body -sS -X "$method" \
    -H "Authorization: Bearer $GH_TOKEN" \
    -H "Accept: application/vnd.github+json" \
    "$@" "$url"
}

# Inhalt einer Datei des Releases (GitHub leitet auf den Speicher weiter)
download() {
  curl -sSfL \
    -H "Authorization: Bearer $GH_TOKEN" \
    -H "Accept: application/octet-stream" \
    "$api/releases/assets/$1"
}

case $1 in
  prepare)
    version=$2 sha=$3 prerelease=$4

    # Entwürfe früherer, abgebrochener Läufe mit diesem Tag entfernen
    list=$(call GET "$api/releases?per_page=100")
    for old in $(echo "$list" | jq -r --arg tag "v$version" '.[] | select(.draft and .tag_name == $tag) | .id'); do
      call DELETE "$api/releases/$old" > /dev/null
      echo "Alten Entwurf $old entfernt" >&2
    done

    body=$(jq -n --arg tag "v$version" --arg sha "$sha" --arg name "BosLook $version" --argjson pre "$prerelease" \
      '{tag_name: $tag, target_commitish: $sha, name: $name, draft: true, prerelease: $pre, generate_release_notes: true}')

    created=$(call POST "$api/releases" --data "$body")
    echo "GitHub create-release response: $created" >&2

    id=$(printf '%s' "$created" | jq -r '.id // empty' 2>/dev/null || {
      echo "Keine gültige JSON-Antwort von GitHub beim Anlegen des Release-Entwurfs." >&2
      printf '%s\n' "$created" >&2
      exit 1
    })

    if [ -z "$id" ]; then
      echo "Entwurf nicht angelegt: $created" >&2
      exit 1
    fi

    echo "$id"
    ;;

  upload)
    id=$2
    shift 2

    # Adresse zum Hochladen steht im Release, z. B. https://uploads.github.com/…/assets{?name,label}
    release=$(call GET "$api/releases/$id")
    uploads=$(echo "$release" | jq -r '.upload_url | sub("\\{.*"; "")')

    for file in "$@"; do
      name=$(basename "$file")
      call POST "$uploads?name=$name" -H "Content-Type: application/octet-stream" --data-binary "@$file" > /dev/null
      echo "Hochgeladen: $name" >&2
    done
    ;;

  publish)
    id=$2
    assets=$(call GET "$api/releases/$id/assets?per_page=100")

    # Jeder Build-Job hat seine Prüfsummen als SHA256SUMS-<target> abgelegt
    : > SHA256SUMS
    for part in $(echo "$assets" | jq -r '.[] | select(.name | startswith("SHA256SUMS-")) | .id'); do
      download "$part" >> SHA256SUMS
      call DELETE "$api/releases/assets/$part" > /dev/null
    done

    cat SHA256SUMS

    if [ ! -s SHA256SUMS ]; then
      echo "Keine Prüfsummen gefunden" >&2
      exit 1
    fi

    "$0" upload "$id" SHA256SUMS

    published=$(call PATCH "$api/releases/$id" --data '{"draft": false}')
    echo "$published" | jq -r '"Veröffentlicht: \(.html_url)"'
    ;;

  *)
    echo "Unbekannter Befehl: $1" >&2
    exit 1
    ;;
esac
