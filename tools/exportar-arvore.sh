#!/usr/bin/env bash
# Exporta a árvore de fontes do Quall Monitor para compilar no Mac ou no Windows.
#
# Gera, na pasta de destino:
#   quall-monitor-<versão>-src.tar.gz   a árvore sem .git e sem target/, com SOURCE-REVISION.txt
#   quall-monitor-<versão>.patch        as mudanças locais sobre a revisão base (inclui arquivos novos)
#
# SOURCE-REVISION.txt identifica a revisão Git base e declara se havia mudanças locais, com o
# sha256 do patch correspondente. Nada é commitado, empurrado nem publicado.
set -euo pipefail

raiz="$(cd "$(dirname "$0")/.." && pwd)"
destino="${1:?uso: tools/exportar-arvore.sh <pasta-de-destino>}"
versao="$(sed -n 's/^version = "\(.*\)"/\1/p' "$raiz/apps/windows/Cargo.toml" | head -1)"
mkdir -p "$destino"
destino="$(cd "$destino" && pwd)"

cd "$raiz"
base="$(git rev-parse HEAD)"
patch="$destino/quall-monitor-$versao.patch"
{
  git diff --binary HEAD
  git ls-files -o --exclude-standard -z | while IFS= read -r -d '' novo; do
    git diff --binary --no-index /dev/null "$novo" || true
  done
} > "$patch"

estado="limpa"
[ -s "$patch" ] && estado="com mudanças locais"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
pasta="$tmp/quall-monitor-$versao"
mkdir -p "$pasta"
# Arquivos rastreados e novos não ignorados; target/ e segredos ignorados ficam fora.
{ git ls-files -z; git ls-files -o --exclude-standard -z; } | \
  while IFS= read -r -d '' f; do [ -e "$f" ] && printf '%s\0' "$f"; done | \
  tar --null -T - -cf - | tar -xf - -C "$pasta"
cat > "$pasta/SOURCE-REVISION.txt" <<EOF
Quall Monitor $versao
Revisão Git base: $base
Árvore: $estado
Patch local: quall-monitor-$versao.patch sha256=$(sha256sum "$patch" | cut -d' ' -f1)
Repositório público: https://github.com/Quevenapp/Quall-Monitor
EOF
tar -C "$tmp" -czf "$destino/quall-monitor-$versao-src.tar.gz" "quall-monitor-$versao"
sha256sum "$destino/quall-monitor-$versao-src.tar.gz" "$patch"
