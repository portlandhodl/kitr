#!/bin/sh
printf '\033[2J\033[H'
printf 'animation frames: red green blue\n'
printf '\033_Ga=f,i=5,r=0,c=2,s=2,v=2,f=24;/wAA/wAA/wAA/wAA\033\\'
printf '\033_Ga=f,i=5,r=1,c=2,s=2,v=2,f=24;AP8AAP8AAP8AAP8A\033\\'
printf '\033_Ga=f,i=5,r=2,c=2,s=2,v=2,f=24;AAD/AAD/AAD/AAD/\033\\'
printf '\033_Ga=a,i=5,s=3,v=1\033\\'
printf '\033_Ga=p,i=5,c=2,r=2\033\\'
printf '\033[8;1Hanimated image above\n'
sleep 30
