#!/bin/sh
printf '\033[2J\033[H'
printf 'Hyperlinks (OSC 8):\n'
printf 'plain \033]8;;https://example.com/\033\\example link\033]8;;\033\\ plain\n'
printf '\033]8;;https://kitty.org/\033\\kitty website\033]8;;\033\\ and '
printf '\033]8;;file:///tmp/parity\033\\file link\033]8;;\033\\\n'
printf 'second line with \033]8;;https://example.org/path\033\\another target\033]8;;\033\\\n'
sleep 30
