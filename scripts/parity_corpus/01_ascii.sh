#!/bin/sh
printf '\033[2J\033[H'
printf 'ASCII 0123456789\n'
printf 'The quick brown fox jumps over the lazy dog.\n'
printf 'Symbols: ! " # $ & ( ) * + , - . / : ; < = > ? @\n'
printf 'Brackets: [ \\ ] ^ _ ` { | } ~\n'
printf 'line five with trailing spaces   \n'
printf 'last line\n'
sleep 30
