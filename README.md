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
<p align="center"><img width="1110" height="770" alt="Screenshot From 2026-08-10 22-18-40" src="https://github.com/user-attachments/assets/c64b9cd6-fa68-4ea6-a166-fbeb287cc783" />
<img width="1110" height="770" alt="Screenshot From 2026-08-10 22-20-35" src="https://github.com/user-attachments/assets/e9f461f9-2b72-4945-8221-250a5fc91d69" />
<p align="center"><img width="410" height="784" alt="Screenshot From 2026-08-10 22-21-54" src="https://github.com/user-attachments/assets/3f65b835-b7ec-434c-b41d-914c661d7a77" />
<img width="410" height="784" alt="Screenshot From 2026-08-10 22-21-47" src="https://github.com/user-attachments/assets/0edb57f7-4bd4-4b5e-9e2d-79c8d9915dc2" />

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
- Fetch brand logos
- Windows support
- Android support
  
## Disclaimers
- Aureus is for personal portfolio tracking and does not provide financial, investment, tax, or legal advice.
- AI was used as a tool during the development of Aureus. The app's design, direction, feature set, UX decisions, testing, debugging, refinement, and release decisions were made by me. AI-assisted code was reviewed, tested, and iterated on.
