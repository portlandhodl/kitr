#!/bin/sh
printf '\033[2J\033[H'
printf 'normal screen before alternate screen\n'
printf 'this content should not be visible\n'
printf '\033[?1049h'
printf '\033[2J\033[H'
printf 'ALTERNATE SCREEN\n'
printf 'alt content line two\n'
printf 'alt content line three\n'
printf '\033[7;9Halternate cursor position'
sleep 30
