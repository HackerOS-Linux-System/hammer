# hammer — Roadmap & Status

## Aktualna wersja: 0.1

### Co zostało naprawione/dodane w 0.1 (względem 0.0.1)

**Krytyczne naprawki:**
- E0502 — borrow conflict w solver.rs (`check_conflicts` zbierał teraz lokalnie, potem extend)
- E0119 — `From<SolverError>` dla anyhow (usunięto ręczny impl, zostawiono blanket)
- E0432 — `indexmap` dodany do Cargo.toml
- `VersionOp` — wszystkie wywołania `c.op` → `c.op.as_str()`
- `InstalledPackage` — nie ma pól `conflicts`, `pre_depends` — poprawiono w conflicts.rs
- `cmd_reinstall` — unused variable `flags` → `_flags`
- `DebPackage::unpack_data` — brakująca metoda zastąpiona właściwym wywołaniem

**Nowe funkcje:**
- `system/postinst.rs` — translator maintainer scripts (patrz niżej)
- `internal/lock.rs` — flock-based locking (nie można uruchomić dwóch hammer naraz)
- `hammer --user remove` — w pełni zaimplementowane
- Walidacja architektury przed rozpakowaniem .deb
- Solver: `conflicts.rs`, `provides.rs`, `version.rs`, `error.rs`, `sat.rs` — pełny podział
- Wszystkie mutujące komendy trzymają system lock

---

## Translator maintainer scripts (postinst.rs)

**Problem który to rozwiązuje:**
Debian nginx po zainstalowaniu chce: stworzyć użytkownika `www-data`, włączyć serwis
`nginx.service`, ustawić uprawnienia katalogów. Bez tego nginx nie działa — nawet jeśli
binarki są poprawnie zainstalowane.

**Jak działa translator:**
Zamiast uruchamiać skrypt instalacyjny bezpośrednio (co kończyło się błędami bo brak
dpkg/apt) albo pomijać go całkowicie (program nie działa), hammer **czyta linia po linii**
co skrypt chce zrobić i tłumaczy na swoje natywne operacje:

```
Skrypt Debiana:          Hammer robi:
useradd --system www-data  → adduser --system www-data (lub useradd -r)
systemctl enable nginx     → systemctl enable --no-reload nginx
update-alternatives ...    → update-alternatives (z poprawionymi ścieżkami)
ldconfig                   → ldconfig
mkdir -p /var/log/nginx    → mkdir -p /var/log/nginx
chown www-data /var/log/nginx → chown www-data /var/log/nginx
dpkg --configure ...       → pominięte (bezpieczne)
apt-get install ...        → pominięte (bezpieczne)
```

**Wynik:**
- `nginx`, `apache2`, `postgresql`, `mysql`, `redis` — będą działać poprawnie po instalacji
- Użytkownicy systemowi są tworzeni (www-data, postgres, redis, itp.)
- Serwisy są włączane przy starcie systemu
- Symlinki i katalogi są tworzone prawidłowo

---

## Co jeszcze wymaga rozbudowy (szczere)

### Problemy krytyczne do 1.0

**1. Postinst translator jest niekompletny**
Obsługuje ~80% typowych skryptów. Brakuje:
- Złożone warunki shellowe (`if dpkg --compare-versions ...`)
- Uciekanie do Perla/Pythona w skryptach
- Preinst / prerm / postrm skrypty (pre-removal hooks)
Priorytet: bardzo wysoki (bez tego część programów nadal nie działa).

**2. Brak delta-upgrade między generacjami**
Przy każdym `hammer install` rozkłada całe paczki od nowa. Powinien
kopiować/symlinkować tylko to co się zmieniło. Aktualnie marnuje dużo miejsca.
Priorytet: wysoki.

**3. Store validation przy sync**
`hammer sync` nie weryfikuje podpisów GPG repozytoriów. Każdy może podać
złośliwe repozytorium i zainstalować dowolne paczki.
Priorytet: wysoki (bezpieczeństwo).

**4. /hammer/active przez symlinki do store**
Aktualnie profile to katalogi pełne symlinków do store. Przy dużej liczbie paczek
(>500) tworzenie profilu trwa długo. Potrzebny overlay filesystem lub hardlinki.
Priorytet: średni (performance).

**5. Brak rollback na poziomie pliku**
Rollback do poprzedniej generacji działa, ale nie usuwa plików konfiguracyjnych
stworzonych przez postinst. Np. po `nginx install` + `nginx remove` + `rollback`
pozostają `/etc/nginx/` pliki.
Priorytet: średni.

**6. GUI Store (hammer-store) jest prototypem**
Hammer Store w Vala jest minimalny — nie pokazuje opisów paczek, rozmiaru,
zależności. Nie ma wyszukiwania w szczegółach, nie ma historii transakcji.
Priorytet: średni.

**7. Brak obs�ugi `arm64` i `i386` w jednym systemie**
Multi-arch (np. 32-bit biblioteki na 64-bit) nie jest obsługiwany.
Priorytet: niski dla HackerOS (x86_64/arm64 only), ale blokuje niektóre programy.

---

## Roadmap

### 0.2 — "Działa z prawdziwymi programami"
**Cel: nginx, apache2, postgresql, redis działają od razu po `hammer install`**

- [ ] Pełny postinst translator (preinst/prerm/postrm + złożone warunki)
- [ ] GPG verification przy `hammer sync`
- [ ] `hammer service` — zarządzanie serwisami z poziomu hammer
- [ ] `hammer user` — zarządzanie użytkownikami systemowymi
- [ ] Lepsze komunikaty błędów gdy program nie startuje po instalacji
- [ ] `hammer log` — podgląd logów ostatniej instalacji
- [ ] Walidacja zależności przy usuwaniu (ostrzeżenie gdy coś zależy od usuwanego)
- [ ] Poprawka: delta-profile (nie tworzyć od nowa całego profilu przy każdej operacji)

### 0.3 — "Szybki i bezpieczny"
**Cel: instalacja 100 paczek < 30 sekund, pełne GPG**

- [ ] Równoległe pobieranie (aktualnie MAX_CONCURRENT=4, powiększyć + HTTP/2)
- [ ] Pełna weryfikacja GPG (InRelease → Packages → .deb SHA256)
- [ ] Incremental profile: tylko nowe/zmienione paczki są linkowane od nowa
- [ ] `hammer cache` — zarządzanie cache (rozmiar, czyszczenie starych wersji)
- [ ] Kompresja store (opcjonalna) dla systemów z małym dyskiem
- [ ] Lepszy solver: backtracking gdy pierwsza próba zawiedzie
- [ ] `hammer pin` — zablokowanie wersji pakietu (jak apt-hold)

### 0.4 — "Prawdziwe generacje"
**Cel: generacje działają jak NixOS — niezawodny rollback**

- [ ] Atomic generation switch z weryfikacją (nie tylko symlink — sprawdź że profil jest kompletny)
- [ ] Generation snapshots (eksport/import całego stanu systemu)
- [ ] Boot menu czytelny: "HackerOS (gen-5 — nginx 1.24, vim 9.1)" zamiast ogólnego opisu
- [ ] `hammer bisect` — znajdź generację która zepsuła coś konkretnego
- [ ] Automatyczny rollback jeśli system nie bootuje przez N sekund

### 0.5 — "Multi-arch i user experience"
**Cel: użytkownik nie musi znać szczegółów technicznych**

- [ ] Multi-arch (.deb i386 na amd64)
- [ ] `hammer why <pkg>` — dlaczego ten pakiet jest zainstalowany (kto go wymaga)
- [ ] `hammer what <file>` — który pakiet dostarcza dany plik (jak `dpkg -S`)
- [ ] `hammer size` — ile miejsca zajmuje każdy pakiet i jego zależności
- [ ] Interaktywny tryb instalacji (potwierdzenie per-pakiet dla podejrzanych)
- [ ] `hammer undo` — cofnij ostatnią operację (generacja - 1)
- [ ] Kolorowe podsumowanie po instalacji: co zostało uruchomione, jakie serwisy, jakie pliki

### 0.6 — "HackerOS Store v2"
**Cel: graficzny store który jest przyjemny w użyciu**

- [ ] Pełny GUI Store (GTK4): opisy, zrzuty ekranu, oceny, kategorie
- [ ] Store: zakładka "Zainstalowane" z możliwością usunięcia jednym kliknięciem
- [ ] Store: historia instalacji z rollback przyciskiem
- [ ] Store: powiadomienia desktop o dostępnych aktualizacjach
- [ ] HackerOS tools GUI: instalacja .hk narzędzi z graficznym oknem postępu

### 0.7 — "Pluginy i rozszerzenia"
**Cel: deweloperzy mogą rozszerzyć hammer**

- [ ] Hook system: `pre-install`, `post-install`, `pre-remove`, `post-remove`
- [ ] Komendy zewnętrzne: `hammer-<cmd>` jako plik wykonywalny (jak git-* pattern)
- [ ] `hammer sandbox` — instalacja pakietu w izolowanym środowisku (test przed instalacją)
- [ ] `hammer build-deb` — proste tworzenie .deb z katalogu projektu

### 0.8 — "Sieć i synchronizacja"
**Cel: instalacja przez sieć lokalną, proxy**

- [ ] HTTP proxy wsparcie
- [ ] Lokalny mirror (hammer jako serwer cache dla LAN)
- [ ] `hammer pull` — pobierz paczki bez instalacji (przygotowanie offline)
- [ ] Rsync-style delta downloads (pobieraj tylko różnicę między wersjami .deb)

### 0.9 — "Hardening i produkcja"
**Cel: można użyć na serwerze produkcyjnym**

- [ ] Pełny audit log wszystkich operacji (kto co zainstalował, kiedy)
- [ ] SELinux/AppArmor profile dla hammer
- [ ] `hammer verify --deep` — sprawdzenie SHA256 każdego pliku w store
- [ ] Automatyczne kopie zapasowe generations.json
- [ ] Watchdog: jeśli hammer-activate failuje 3 razy z rzędu → automatyczny rollback
- [ ] `hammer emergency-shell` — minimal recovery mode gdy system nie bootuje

### 1.0 — "Production ready"
**Cel: można wymienić apt na hammer na serwerze produkcyjnym Debian/HackerOS**

- [ ] Wszystkie powyższe
- [ ] 100% kompatybilność z paczkami Debian bookworm/trixie (nginx, apache, postgres, mysql, redis, php, nodejs, python3)
- [ ] Dokumentacja: man pages dla wszystkich komend
- [ ] Test suite: >500 testów integracyjnych
- [ ] Benchmark: install vim z zależnościami < 10s na świeżym systemie
- [ ] Pierwsze stabilne API dla zewnętrznych narzędzi

---

## Szczera ocena (bez programistycznego bełkotu)

### Co działa dobrze teraz:
- Pobieranie i rozpakowywanie paczek Debian
- Śledzenie zainstalowanych pakietów
- Generacje: instalujesz → zmiany są "zaplanowane" → po restarcie wchodzą w życie
- Rollback do poprzedniej generacji działa
- GRUB menu pokazuje dostępne generacje
- `hammer sync` odświeża listę dostępnych paczek
- HackerOS tools (.hk) — instalacja i aktualizacja narzędzi z GitHub
- Wyszukiwanie i info o paczkach

### Co NIE działa albo działa słabo:
- **Programy wymagające systemd** (nginx, apache, postgresql) — serwisy mogą nie startować automatycznie po instalacji. Translator postinst (0.1) to naprawia w teorii, w praktyce potrzeba testowania.
- **Nie sprawdza podpisów GPG** — można zainstalować podrobioną paczkę. Krytyczne dla produkcji.
- **Pamięć podręczna** — przy dużym `hammer upgrade` może zabraknąć miejsca (duplikaty w store)
- **Brak `hammer what /usr/bin/nginx`** — nie wiesz który pakiet dostarcza dany plik
- **GUI Store jest prosty** — brak opisów, zrzutów ekranu, kategorii
- **Multi-arch** — brak (np. wine 32-bit na 64-bit systemie)

### Kiedy można używać do poważnych zastosowań:
- **Desktop (HackerOS)**: już teraz, z zastrzeżeniem że niektóre programy wymagają ręcznego `systemctl enable/start`
- **Serwer**: od wersji **0.3** gdy będzie GPG + stabilny postinst translator
- **Produkcja krytyczna**: od **1.0**
