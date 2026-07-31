## Active report language: LaTeX

Write the report entry point as `Report/main.tex`. Load `latex-compile` before compiling or diagnosing a PDF.

### LaTeX layout rules

- Prefer compact `l`/`c`/`r` table columns; use `tabularx` or explicit widths only when needed.
- Use `[H]` for figures and tables when fixed source placement is required; this relies on the `float` package.
- Use `\graphicspath{{../Plots/Fig/}}`, `\includegraphics`, `.bib` bibliography resources, and standard LaTeX commands.
