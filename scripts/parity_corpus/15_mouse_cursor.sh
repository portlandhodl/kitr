#!/bin/sh
printf '\033[2J\033[H'
printf 'mouse cursor / pointer shape\n'
printf 'hover target: \033]8;;https://example.com/hover\033\\hover link\033]8;;\033\\\n'
printf '\033]22;pointer\033\\'
printf 'pointer set to pointer shape\n'
sleep 30
