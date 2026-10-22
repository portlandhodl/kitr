#!/bin/sh
printf '\033[2J\033[H'
printf 'scroll test begins\n'
i=1
while [ "$i" -le 45 ]; do
    printf 'scroll line %02d\n' "$i"
    i=$((i + 1))
done
printf '\033[24;1Hbottom marker after scrolling'
sleep 30
