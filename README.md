# Aureus 

Aureus is simple portfolio tracker built for Linux desktop and mobile devices.

## Features
- Multiple accounts
- Portfolio history
- Trades and cash activity
- Dividend tracking
- Stock search and watchlist
- Reports available as a PDF export

## Screenshots
<div align="center">
  <img width="1183" alt="Screenshot From 2026-08-21 14-22-51" src="https://github.com/user-attachments/assets/d379c477-b03b-45ce-8f4e-5a998c46fb94" style="background: transparent;" />

  <img width="1183" alt="Screenshot From 2026-08-21 14-23-00" src="https://github.com/user-attachments/assets/fdbaa30b-0146-4844-ad06-1a1acb247ec7" style="background: transparent;" />

  <br />

  <img width="410" alt="Screenshot From 2026-08-21 14-23-16" src="https://github.com/user-attachments/assets/bb636df6-bd4c-4cb0-b43f-6d4733b732f3" />
  &nbsp;&nbsp;
  <img width="410" alt="Screenshot From 2026-08-21 14-23-21" src="https://github.com/user-attachments/assets/c7f1e319-8c32-4b7c-94d7-4a4fa17c917e" />
</div>

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
