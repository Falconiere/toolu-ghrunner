#!/usr/bin/env bash
set -euo pipefail

printf '%s\n' '::add-mask::probe-mask-82'
printf '%s\n' '::error file=src%2Csample.rs,line=4,col=2,endLine=4,endColumn=8,title=Compile%3A detail::boom%0Aprobe-mask-82'
printf '%s\n' '::warning file=src/probe-mask-82.rs,line=9,title=Warn%3Aprobe-mask-82::warning text'
printf '%s\n' '::notice::plain notice'
printf '%s\n' '::warning endLine=7::end line only'
printf '%s\n' '::warning line=3,endLine=2::descending lines'
printf '%s\n' '::warning line=4,endLine=5,col=2,endColumn=8::multiline drops columns'
printf '%s\n' '::warning line=4,endColumn=9::end column only'
printf '%s\n' '::warning col=3::column without line'
printf '%s\n' '::warning line=4,col=9,endColumn=2::descending columns'
printf '%s\n' '::warning line=oops::invalid line'
printf '%s\n' '::notice title=ignored::   '
