# Build the paper. `make` here is independent of the workspace build.
#
# The acmart class is vendored under cls/ rather than assumed present:
# texlive-publishers is not in every distribution's base install, and a paper
# that only builds on the author's machine is a paper nobody else can check.

TEXINPUTS := ./cls//:
BSTINPUTS := ./cls//:
export TEXINPUTS BSTINPUTS

.PHONY: all clean pages figures todos

all: paper.pdf

# Regenerate figures from committed measurement data. This does NOT re-measure
# -- see the `measure_*` targets in the repo root for that, which need a live
# server, model cache and external sources. Splitting the two means a paper
# rebuild is offline and deterministic.
figures:
	@echo "no generated figures yet -- see plan.md H-8"

paper.pdf: paper.tex refs.bib
	pdflatex -interaction=nonstopmode -halt-on-error paper.tex
	-bibtex paper
	pdflatex -interaction=nonstopmode -halt-on-error paper.tex
	pdflatex -interaction=nonstopmode -halt-on-error paper.tex
	@$(MAKE) --no-print-directory pages
	@$(MAKE) --no-print-directory todos

# The page budget is 8-10. Print it after every build so it cannot drift
# unnoticed while sections are being written.
pages: paper.pdf
	@n=$$(pdfinfo paper.pdf 2>/dev/null | awk '/^Pages:/ {print $$2}'); \
	 echo "==> $$n pages (budget 8-10)"

# Unmeasured numbers render in red as [? ...]. Count them after every build so
# the manuscript cannot ship with one unnoticed.
todos:
	@n=$$(grep -c '\\todonum{' paper.tex || true); \
	 echo "==> $$n unmeasured numbers (\\todonum) -- must be 0 to submit"

clean:
	rm -f paper.pdf paper.aux paper.log paper.out paper.bbl paper.blg
