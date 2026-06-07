" Filetype detection for Leekscript sources.
" .leek is unambiguous; .lk and .ls are also mapped to Leekscript
" (note: .ls would otherwise be detected as LiveScript).
au BufRead,BufNewFile *.leek,*.lk,*.ls set filetype=leek
