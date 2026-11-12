#!/bin/sh
# Установка SafeDrop Server на Linux с systemd. Запуск от root:
#   sudo sh install.sh [путь-к-safedrop-server]
# Без пути берёт ./safedrop-server рядом со скриптом, затем target/release,
# а если есть cargo — собирает сам. Повторный запуск обновляет сервер,
# пароль и ключ сервера при этом сохраняются.
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
BIN="${1:-}"

if [ "$(id -u)" != 0 ]; then
  echo "Запусти от root: sudo sh $0" >&2
  exit 1
fi
if ! command -v systemctl >/dev/null 2>&1; then
  echo "Нужен systemd (systemctl не найден)." >&2
  exit 1
fi

if [ -z "$BIN" ]; then
  if [ -x "$HERE/safedrop-server" ]; then
    BIN="$HERE/safedrop-server"
  elif [ -x "$HERE/target/release/safedrop-server" ]; then
    BIN="$HERE/target/release/safedrop-server"
  elif command -v cargo >/dev/null 2>&1; then
    (cd "$HERE" && cargo build --release)
    BIN="$HERE/target/release/safedrop-server"
  else
    echo "Нет файла safedrop-server и нет cargo, чтобы его собрать." >&2
    echo "Положи готовый safedrop-server рядом со скриптом или укажи путь: sudo sh $0 /путь/safedrop-server" >&2
    exit 1
  fi
fi

# бинарник вообще запускается на этой системе?
"$BIN" --version

install -m 0755 "$BIN" /usr/local/bin/safedrop-server
install -m 0644 "$HERE/safedrop-server.service" /etc/systemd/system/safedrop-server.service

ENV=/etc/safedrop-server.env
if [ ! -f "$ENV" ]; then
  PASS=$(head -c 24 /dev/urandom | base64 | tr -d '/+=' | cut -c1-20)
  umask 077
  cat > "$ENV" <<EOF
# Пароль для входа: его вписывают в SafeDrop во вкладке «Сервер».
SAFEDROP_SERVER_PASSWORD=$PASS
# Дополнительные параметры (safedrop-server --help). Если у сервера белый IP
# не на сетевой карте (облако с NAT), укажи его для звонков:
# SAFEDROP_SERVER_ARGS=--public-ip 203.0.113.5
SAFEDROP_SERVER_ARGS=
EOF
  chmod 600 "$ENV"
  echo "Создан $ENV с новым паролем."
fi

# порты: TCP 47840 — приложения, UDP 3478 — звонки (TURN). Внутренние порты TURN
# (49160-49400) снаружи не нужны: сервер пересылает между ними сам, через 127.0.0.1
if command -v ufw >/dev/null 2>&1 && ufw status 2>/dev/null | grep -q "Status: active"; then
  ufw allow 47840/tcp
  ufw allow 3478/udp
  echo "Порты открыты в ufw."
elif command -v firewall-cmd >/dev/null 2>&1 && firewall-cmd --state >/dev/null 2>&1; then
  firewall-cmd --permanent --add-port=47840/tcp --add-port=3478/udp
  firewall-cmd --reload
  echo "Порты открыты в firewalld."
else
  echo "ufw/firewalld не найдены. Если есть другой файрвол (iptables, панель хостинга) —"
  echo "открой TCP 47840 и UDP 3478."
fi

systemctl daemon-reload
systemctl enable safedrop-server >/dev/null
systemctl restart safedrop-server
sleep 2
systemctl --no-pager --lines=8 status safedrop-server || true

IP=$(ip -4 route get 8.8.8.8 2>/dev/null | awk '{for (i = 1; i <= NF; i++) if ($i == "src") print $(i + 1)}' | head -n1)
FP=""
if [ -f /var/lib/safedrop-server/server-cert.der ]; then
  FP=$(/usr/local/bin/safedrop-server --data /var/lib/safedrop-server --fingerprint 2>/dev/null || true)
fi
PASS=$(sed -n 's/^SAFEDROP_SERVER_PASSWORD=//p' "$ENV" | head -n1)

echo
echo "Готово. В SafeDrop: вкладка «Сервер» → «Через сервер»:"
echo "  адрес:  ${IP:-<белый IP сервера>}"
echo "  пароль: ${PASS:-<не задан>}"
[ -n "$FP" ] && echo "  ключ сервера (сверить с тем, что покажет SafeDrop): $(echo "$FP" | cut -c1-32 | sed -E 's/.{4}/& /g')…"
echo "Журнал: journalctl -u safedrop-server -f"
