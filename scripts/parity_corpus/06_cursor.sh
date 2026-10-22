#!/bin/sh
printf '\033[2J\033[H'
printf 'cursor addressing\n'
printf '\033[3;5H@\033[3;15H@\033[3;25H@'
printf '\033[5;1Hleft edge'
printf '\033[7;70Hright edge'
printf '\033[9;10H1234567890'
printf '\033[11;10H\033[2Dleft of text'
printf '\033[13;1Hrelative moves: \033[3Cright\033[2D\033[1Aup'
printf '\033[18;8Hfinal cursor position'
sleep 30
