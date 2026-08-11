# Aureus 

Aureus is a GTK4 and libadwaita portfolio tracker built for Linux desktop and mobile devices.

## Features
- Multiple accounts
- Portfolio history
- Trades and cash activity
- Dividend tracking
- Stock search and watchlist
- Reports available as a PDF export

## Screenshots
<img width="1310" height="744" alt="Screenshot From 2026-08-10 16-33-26" src="https://github.com/user-attachments/assets/607a27ff-5f91-4d12-be42-80d6ed7fc640" />
<img width="1310" height="744" alt="Screenshot From 2026-08-10 16-34-17" src="https://github.com/user-attachments/assets/031bc9f2-4332-48d1-a801-824ffe9bb46a" />
<img width="410" height="774" alt="Screenshot From 2026-08-10 16-37-40" src="https://github.com/user-attachments/assets/4de6237d-1ce8-4a5c-a446-e3a27856de4e" />
<img width="410" height="774" alt="Screenshot From 2026-08-10 16-37-37" src="https://github.com/user-attachments/assets/18b6c44e-3fc8-4ce1-bb0e-429bdfd24578" />



## Build

Run:

```
git clone https://github.com/Mars7x/Aureus.git && cd Aureus && flatpak-builder build-dir io.github.Mars7x.Aureus.yml
```
Alternatively, use [Builder](https://apps.gnome.org/Builder/)

## Built with
- GTK4 and libadwaita for UI
- Yahoo Finance for market data
- Bank of Canada for CAD/USD exchange rates
- SQLite for local portfolio storage
- GPT 5.6 Sol for logic
- Used [Planify](https://flathub.org/apps/io.github.alainm23.planify) colour scheme for libadwaita colours

## Roadmap
- Figure out a way to reliably fetch company icons and remove manually setting icons.
  
## Disclaimers
- Aureus is for personal portfolio tracking and does not provide financial, investment, tax, or legal advice.
- AI was used as a tool during the development of Aureus. The app's design, direction, feature set, UX decisions, testing, debugging, refinement, and release decisions were made by me. AI-assisted code was reviewed, tested, and iterated on.
