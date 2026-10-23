#!/bin/sh
printf '\033[2J\033[H'
printf 'normal text\n'
printf '\033]66;w=2;s=2;SIZE\a'
printf ' after scaled text\n'
printf '\033]66;w=3;s=1;wide\a'
printf ' after wide text\n'
printf '\033]66;w=1;s=2;high\a'
printf '\n'
sleep 30
