#!/system/bin/sh
# =====================================================================
# deploy_redwood.sh — установка собственного thermal-движка (mi_thermald,
# пересобранного на Rust) на POCO X5 Pro (board redwood, SM7325/778G)
# с сохранением оригинального mi_thermald как страховки и полным откатом.
#
# Запуск НА ТЕЛЕФОНЕ (root):
#   sh /data/local/tmp/deploy_redwood.sh test          # тест без отключения mi_thermald
#   sh /data/local/tmp/deploy_redwood.sh install       # замена бинаря (после успешного test)
#   sh /data/local/tmp/deploy_redwood.sh restore       # полный откат к оригинальному
#   sh /data/local/tmp/deploy_redwood.sh status        # состояние
#
# Требования: root (Magisk/KernelSU), бинарь заранее положен в
#   /data/local/tmp/mi_thermald
# =====================================================================
set -u

BIN=/data/local/tmp/mi_thermald
DST=/system/vendor/bin/mi_thermald        # путь из init.mi_thermald.rc
RC=/vendor/etc/init/hw/init.mi_thermald.rc
BK=/data/vendor/thermal/mi_thermald.backup
CTX=u:object_r:mi_thermald_exec:s0

cmd="${1:-status}"
[ "$(id -u)" = "0" ] || { echo "NOT ROOT — запусти через su"; exit 1; }

board="$(getprop ro.product.board)"
soc="$(getprop ro.soc.model)"
echo "== board=$board soc=$soc model=$(getprop ro.product.model)"

case "$cmd" in
  status)
    echo "-- процесс:"; ps -A -o PID,NAME | grep -E "mi_thermald|thermal-engine" || echo "  mi_thermald НЕ запущен"
    echo "-- файлы:"; ls -lZ $DST $RC $BIN 2>/dev/null
    echo "-- режим: persist.vendor.mi_thermal_ai=$(getprop persist.vendor.mi_thermal_ai)"
    echo "-- следы логирования:"; logcat -d -s thermald 2>/dev/null | tail -n 5
    ;;

  test)
    echo "== ТЕСТ: запуск нашего демона на 20 сек, mi_thermald НЕ трогаем"
    pkill -f "$BIN" 2>/dev/null
    setprop persist.mithermal.debug 1
    "$BIN" -l 7 &
    TPID=$!
    sleep 20
    echo "-- наш процесс: $TPID"; ps -p $TPID -o PID,NAME,STAT || echo "  упал!"
    echo "-- последние строки thermald:"
    logcat -d -s thermald 2>/dev/null | tail -n 40
    echo "-- частоты CPU сейчас (должны быть на капах, не на макс):"
    for p in /sys/devices/system/cpu/cpufreq/policy*/scaling_cur_freq; do
      echo "  $p = $(cat $p)"
    done
    kill $TPID 2>/dev/null; wait $TPID 2>/dev/null
    echo "== ТЕСТ завершён. Смотри вывод выше: канал discovery и ошибок быть не должно."
    ;;

  install)
    [ "$board" = "redwood" ] || { echo "ОШИБКА: board '$board' != redwood. Прерываю."; exit 1; }
    [ -f "$BIN" ] || { echo "ОШИБКА: нет $BIN (положи бинарь)."; exit 1; }
    echo "== Бэкап оригинального mi_thermald в $BK"
    mkdir -p "$BK/bin" "$BK/init" "$BK/vendor_etc"
    setprop persist.vendor.mi_thermal_ai true   # принудительно AI-native (наш режим)
    setprop persist.mithermal.debug 1

    stop mi_thermald
    sleep 1
    # Бэкапим оригинал ТОЛЬКО при первой установке — повторный install
    # обновляет наш бинарь и не должен затирать сохранённый оригинал.
    if [ -f "$BK/bin/original_mi_thermald" ]; then
      echo "  backup bin уже есть ($BK/bin/original_mi_thermald) — сохраняю как есть"
    elif cp -a "$DST" "$BK/bin/original_mi_thermald" 2>/dev/null; then
      echo "  backup bin OK"
    else
      echo "  WARN: не смог скопировать $DST (продолжаю)"
    fi
    if [ -f "$BK/init/init.mi_thermald.rc" ]; then
      echo "  backup rc уже есть — сохраняю как есть"
    elif cp -a "$RC" "$BK/init/init.mi_thermald.rc" 2>/dev/null; then
      echo "  backup rc OK"
    else
      echo "  WARN: rc не скопирован (не критично)"
    fi
    cp -a /vendor/etc/thermal-*.conf "$BK/vendor_etc/" 2>/dev/null

    echo "== Монтируем /system в RW (erofs может не дать — тогда нужен overlayfs/KernelSU)"
    mount -o rw,remount /system 2>/dev/null || mount -o rw,remount / 2>/dev/null
    mount | grep -E "on /system " | head -n 1

    if cp "$BIN" "$DST" 2>/dev/null; then
      chown root:shell "$DST"
      chmod 0755 "$DST"
      chcon "$CTX" "$DST" 2>/dev/null || echo "  WARN: chcon не удался (проверь контекст)"
      echo "== Бинарь заменён: $DST"
    else
      echo "!! ОШИБКА записи в $DST (раздел read-only)."
      echo "   Варианты: 1) KernelSU overlayfs: ksud overlayfs enable + reboot"
      echo "             2) править образ vendor/system и прошить"
      echo "   Откат не нужен — оригинал не тронут."
      setprop persist.vendor.mi_thermal_ai false
      start mi_thermald
      exit 1
    fi

    echo "== Запуск нашего движка"
    start mi_thermald
    sleep 4
    echo "-- процесс:"; ps -A -o PID,NAME | grep mi_thermald || echo "  НЕ запустился!"
    echo "-- лог:"; logcat -d -s thermald 2>/dev/null | tail -n 30
    echo "== Установка завершена. Откат: sh /data/local/tmp/deploy_redwood.sh restore"
    ;;

  restore)
    echo "== Откат к оригинальному mi_thermald"
    stop mi_thermald
    sleep 1
    mount -o rw,remount /system 2>/dev/null || mount -o rw,remount / 2>/dev/null
    if [ -f "$BK/bin/original_mi_thermald" ]; then
      cp -a "$BK/bin/original_mi_thermald" "$DST" && echo "  бинарь восстановлен"
      chown root:shell "$DST"; chmod 0755 "$DST"
      chcon "$CTX" "$DST" 2>/dev/null || true
    else
      echo "  Нет бэкапа бинаря ($BK/bin/original_mi_thermald) — восстановление невозможно, прошей vendor/system"
      setprop persist.vendor.mi_thermal_ai false
      start mi_thermald
      exit 1
    fi
    [ -f "$BK/init/init.mi_thermald.rc" ] && cp -a "$BK/init/init.mi_thermald.rc" "$RC" 2>/dev/null
    setprop persist.vendor.mi_thermal_ai false
    setprop persist.mithermal.debug 0
    start mi_thermald
    sleep 3
    echo "-- процесс:"; ps -A -o PID,NAME | grep mi_thermald
    echo "== Откат завершён."
    ;;

  *)
    echo "Использование: sh $0 {status|test|install|restore}"
    ;;
esac