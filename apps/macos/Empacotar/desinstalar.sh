#!/bin/bash
# Desinstalador do Quall Monitor para macOS. Remove somente o que pertence ao Monitor
# (bundle br.com.queven.quall.monitor); nunca toca no Quall Studio nem nos dados dele.
#
#   desinstalar.sh              remove o app em /Applications e o recibo do PKG
#   desinstalar.sh --dados      também apaga pareamentos, sessões salvas, preferências e logs do Monitor
#   desinstalar.sh --simular    só mostra o que faria
#
# QUALL_DESINSTALAR_RAIZ=/caminho prefixa /Applications e a pasta pessoal (validação de bancada).
set -euo pipefail
BUNDLE_ID="br.com.queven.quall.monitor"
PKG_ID="br.com.queven.quall.monitor.pkg"
RAIZ="${QUALL_DESINSTALAR_RAIZ:-}"
PESSOAL="${RAIZ:+$RAIZ/home}"
PESSOAL="${PESSOAL:-$HOME}"
APP="$RAIZ/Applications/Quall Monitor.app"
DADOS=nao
SIMULAR=nao
for ARG in "$@"; do
    case "$ARG" in
        --dados|--remover-dados) DADOS=sim ;;
        --simular) SIMULAR=sim ;;
        -h|--help) sed -n '2,9p' "$0"; exit 0 ;;
        *) echo "Opção desconhecida: $ARG" >&2; exit 2 ;;
    esac
done

fazer() {
    printf '  %s\n' "$*"
    [ "$SIMULAR" = sim ] || "$@"
}

remover() {
    local ALVO="$1"
    [ -e "$ALVO" ] || [ -L "$ALVO" ] || return 0
    if [ -w "$(dirname "$ALVO")" ]; then
        fazer rm -rf "$ALVO"
    else
        fazer sudo rm -rf "$ALVO"
    fi
}

# Monitores virtuais pertencem ao helper de cada sessão: são removidos quando o app encerra.
if [ -z "$RAIZ" ] && pgrep -f "Quall Monitor.app/Contents/MacOS/quall-monitor-(app|display)" >/dev/null 2>&1; then
    echo "O Quall Monitor ainda está aberto. Desconecte os monitores, feche o app e rode de novo." >&2
    exit 1
fi

MODO=""
[ "$SIMULAR" = sim ] && MODO=" [simulação]"
echo "Quall Monitor — desinstalação${RAIZ:+ (raiz $RAIZ)}$MODO"
if [ -d "$APP" ]; then
    ID="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$APP/Contents/Info.plist" 2>/dev/null || true)"
    if [ "$ID" != "$BUNDLE_ID" ]; then
        echo "Recusado: $APP tem bundle '$ID', não $BUNDLE_ID." >&2
        exit 1
    fi
    remover "$APP"
else
    echo "  App não encontrado em $APP"
fi

if [ -z "$RAIZ" ] && pkgutil --pkg-info "$PKG_ID" >/dev/null 2>&1; then
    fazer sudo pkgutil --forget "$PKG_ID"
fi

if [ "$DADOS" = sim ]; then
    remover "$PESSOAL/Library/Application Support/Quall Monitor"
    remover "$PESSOAL/Library/Logs/Quall Monitor"
    remover "$PESSOAL/Library/Caches/$BUNDLE_ID"
    remover "$PESSOAL/Library/Saved Application State/$BUNDLE_ID.savedState"
    if [ -z "$RAIZ" ]; then
        if defaults read "$BUNDLE_ID" >/dev/null 2>&1; then fazer defaults delete "$BUNDLE_ID"; fi
    fi
    remover "$PESSOAL/Library/Preferences/$BUNDLE_ID.plist"
else
    echo "  Dados preservados (pareamentos, sessões salvas, logs). Use --dados para apagá-los."
fi
echo "Concluído."
