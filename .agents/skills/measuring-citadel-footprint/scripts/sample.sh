#!/bin/zsh
# Sample one process about once per 1.5 s until it exits or SAMPLES are taken.
# usage: zsh sample.sh PID OUT.csv SAMPLES
# Columns: epoch, rss_kb (ps), cpu_pct (ps decaying average; do not report),
#          cputime (ps TIME, cumulative), threads, footprint (top MEM, e.g. 4720K / 27M),
#          compressed (top CMPRS).
pid=$1; out=$2; n=$3
echo "epoch,rss_kb,cpu_pct,cputime,threads,footprint,compressed" > "$out"
for i in $(seq 1 "$n"); do
  line=$(ps -o rss=,%cpu=,time= -p "$pid") || break
  th=$(ps -M -p "$pid" | tail -n +2 | wc -l | tr -d ' ')
  mem=$(top -l 1 -pid "$pid" -stats mem,cmprs | tail -1 | awk '{print $1","$2}')
  echo "$(date +%s),$(echo $line | awk '{print $1","$2","$3}'),$th,$mem" >> "$out"
  sleep 1
done
