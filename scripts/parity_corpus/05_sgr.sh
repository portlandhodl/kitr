#!/bin/sh
printf '\033[2J\033[H'
printf 'SGR attributes:\n'
printf '\033[1mBold\033[0m \033[2mDim\033[0m \033[3mItalic\033[0m \033[4mUnderline\033[0m \033[7mReverse\033[0m \033[9mStrike\033[0m\n'
printf 'Combined: \033[1;3;4;31mBold italic underlined red\033[0m\n\n'
printf 'Standard foreground 30-37:\n'
i=30
while [ "$i" -le 37 ]; do
    printf '\033[%dm[%02d]\033[0m ' "$i" "$i"
    i=$((i + 1))
done
printf '\nBright foreground 90-97:\n'
i=90
while [ "$i" -le 97 ]; do
    printf '\033[%dm[%02d]\033[0m ' "$i" "$i"
    i=$((i + 1))
done
printf '\nBackground 40-47:\n'
i=40
while [ "$i" -le 47 ]; do
    printf '\033[%dm  \033[0m' "$i"
    i=$((i + 1))
done
printf '\nBright background 100-107:\n'
i=100
while [ "$i" -le 107 ]; do
    printf '\033[%dm  \033[0m' "$i"
    i=$((i + 1))
done
printf '\n256-colour samples:\n'
i=16
while [ "$i" -le 51 ]; do
    printf '\033[48;5;%dm \033[0m' "$i"
    i=$((i + 6))
done
printf '\nTruecolour RGB strip:\n'
printf '\033[48;2;255;0;0m  \033[0m'
printf '\033[48;2;255;128;0m  \033[0m'
printf '\033[48;2;255;255;0m  \033[0m'
printf '\033[48;2;0;255;0m  \033[0m'
printf '\033[48;2;0;128;255m  \033[0m'
printf '\033[48;2;128;0;255m  \033[0m\n'
sleep 30
