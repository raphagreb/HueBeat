/*
  Album color on the LED + title/artist/progress on the OLED + beat mode + idle screen
  OLED: SDA->A4, SCL->A5 (0x3C) | RGB LED: R->9, G->10, B->11 | LDR: A3 | DHT11: A1

  Power note: the LED is capped by MAX_BRIGHTNESS because a high-current LED
  fed from the Uno's 5V pin browns out the board and the USB serial link
  disconnects. For full brightness, power the LED from a separate 5V supply
  (common ground) and raise MAX_BRIGHTNESS.

  Serial protocol (newline-terminated lines), sent by the Python app:
    C,r,g,b     target color
    T,text      title (empty text = no track)
    A,text      artist
    S,1/0       playing / paused
    P,pos,dur   track position and duration, in seconds
    M,1/0       beat mode on / off
    B           a beat happened (flash)
    I,1/0       idle (no media session) on / off
    K,HH:MM[:SS] current time (clock sync)
    J,dd/mm/yyyy current date
    W,Mon       weekday (Mon..Sun)
*/
#include <Wire.h>
#include <Adafruit_GFX.h>
#include <Adafruit_SSD1306.h>
#define USE_DHT 1   // 0 = disable the DHT11 completely (for troubleshooting)
#if USE_DHT
#include <DHT.h>
#endif
#include <math.h>

#define USE_LDR true
#define DHTPIN  A1
#define DHTTYPE DHT11

// ---- Color adjustment ----
const float GAMMA = 2.2;
const float CAL_R = 1.00;
const float CAL_G = 0.85;
const float CAL_B = 0.85;
// ---- Brightness (0 to 1, applied after gamma) ----
const float MAX_BRIGHTNESS    = 0.5;    // global cap; lower it if the board resets when the LED pulses (USB power limit)
const float PAUSED_BRIGHTNESS = 0.15;   // brightness when the music is paused (0 = off)
const float BEAT_MIN          = 0.25;   // brightness between beats in beat mode
// ---- Idle screen ----
const unsigned long IDLE_AFTER_PAUSE_MS = 30000;  // paused this long -> idle screen (0 = never, only the I flag)
const bool IDLE_LED = true;                       // breathing LED while idle
const byte IDLE_R = 255, IDLE_G = 110, IDLE_B = 30;  // idle LED color (warm)
const unsigned long DHT_INTERVAL = 5000;          // DHT11 read interval (ms)
// -----------------------

Adafruit_SSD1306 oled(128, 64, &Wire, -1);
#if USE_DHT
DHT dht(DHTPIN, DHTTYPE);
#endif

const int pinRed   = 9;
const int pinGreen = 10;
const int pinBlue  = 11;
const int ldr      = A3;

byte targetR = 0, targetG = 0, targetB = 0;
float curR = 0, curG = 0, curB = 0;
float brightness = 0;
bool playing = false;
bool beatOn = false;
bool hasTrack = false;
float flash = 0;
unsigned long lastBeatMs = 0;   // last time a "B" arrived (beat activity watchdog)

long basePos = 0;
long duration = 0;
unsigned long posRef = 0;
unsigned long pausedSince = 0;

// Clock (synced by the Python app, ticks on its own between syncs)
bool clockOk = false;
bool secKnown = false;
byte clkH = 0, clkM = 0, clkS = 0;
unsigned long clkRef = 0;
char dateStr[11] = "";
char wdayStr[4] = "";
bool idleFlag = false;   // set by the Python app (no media session)

// DHT
bool dhtOk = false;
bool dhtTried = false;
int tempC = 0, hum = 0;
unsigned long lastDht = 0;

char title[40] = "";
char artist[32] = "";
char line[56];
byte lineLen = 0;

unsigned long lastOled = 0, lastFade = 0, scrollStart = 0;

int freeRam() {
  extern int __heap_start, *__brkval;
  int v;
  return (int)&v - (__brkval == 0 ? (int)&__heap_start : (int)__brkval);
}

void setup() {
  // Sign of life: 3 quick blinks of the on-board LED (pin 13)
  pinMode(LED_BUILTIN, OUTPUT);
  for (byte i = 0; i < 3; i++) {
    digitalWrite(LED_BUILTIN, HIGH); delay(120);
    digitalWrite(LED_BUILTIN, LOW);  delay(120);
  }

  Serial.begin(115200);

  pinMode(pinRed, OUTPUT);
  pinMode(pinGreen, OUTPUT);
  pinMode(pinBlue, OUTPUT);
  pinMode(ldr, INPUT);

#if USE_DHT
  dht.begin();
#endif

  if (!oled.begin(SSD1306_SWITCHCAPVCC, 0x3C)) {
    // OLED failed (wiring/address or out of RAM): blink the on-board LED forever
    pinMode(LED_BUILTIN, OUTPUT);
    for (;;) {
      digitalWrite(LED_BUILTIN, !digitalRead(LED_BUILTIN));
      delay(150);
    }
  }
  Wire.setClock(400000);
#if defined(WIRE_HAS_TIMEOUT)
  Wire.setWireTimeout(3000, true);   // recover I2C if it hangs
#endif
  oled.setTextColor(WHITE);
  oled.setTextWrap(false);

  // Boot screen: shows the free RAM left (should be > 250)
  oled.clearDisplay();
  oled.setTextSize(1);
  oled.setCursor(0, 0);
  oled.print(F("OLED OK"));
  oled.setCursor(0, 12);
  oled.print(F("Free RAM: "));
  oled.print(freeRam());
  oled.display();
  delay(2500);

  // Handshake: the Python app waits for this line before sending commands,
  // so nothing is lost while the board is still booting.
  Serial.println(F("READY"));
}

void loop() {
  readSerial();
  updateDht();
  updateLed();

  if (millis() - lastOled > 50) {
    lastOled = millis();
    drawScreen();
  }
}

// ======================= State =======================

bool isIdle() {
  if (playing) return false;
  if (idleFlag || !hasTrack) return true;
  if (IDLE_AFTER_PAUSE_MS == 0) return false;
  return (millis() - pausedSince) > IDLE_AFTER_PAUSE_MS;
}

// ======================= Serial =======================

void copyText(char *dest, size_t size, const char *src) {
  strncpy(dest, src, size - 1);
  dest[size - 1] = '\0';
  scrollStart = millis();
}

void processLine() {
  line[lineLen] = '\0';
  lineLen = 0;

  // Beat: a line containing only "B"
  if (line[0] == 'B') {
    if (line[1] == '\0') {
      flash = 1.0;
      lastBeatMs = millis();
      return;
    }
  }

  if (line[1] != ',') return;

  char type = line[0];
  const char *data = line + 2;

  if (type == 'C') {
    int r, g, b;
    if (sscanf_P(data, PSTR("%d,%d,%d"), &r, &g, &b) == 3) {
      targetR = constrain(r, 0, 255);
      targetG = constrain(g, 0, 255);
      targetB = constrain(b, 0, 255);
    }
  } else if (type == 'T') {
    hasTrack = (data[0] != '\0');
    copyText(title, sizeof(title), data);
  } else if (type == 'A') {
    copyText(artist, sizeof(artist), data);
  } else if (type == 'S') {
    bool was = playing;
    playing = (data[0] == '1');
    if (was && !playing) pausedSince = millis();
  } else if (type == 'M') {
    beatOn = (data[0] == '1');
    if (!beatOn) flash = 0;   // turning beat mode off stops the pulse; a periodic M,1 keeps it
  } else if (type == 'P') {
    long p, d;
    if (sscanf_P(data, PSTR("%ld,%ld"), &p, &d) == 2) {
      basePos = p;
      duration = d;
      posRef = millis();
    }
  } else if (type == 'I') {
    idleFlag = (data[0] == '1');
  } else if (type == 'K') {
    int h, m, s = 0;
    int n = sscanf_P(data, PSTR("%d:%d:%d"), &h, &m, &s);
    if (n >= 2) {
      clkH = h % 24;
      clkM = m % 60;
      clkS = (n == 3) ? s % 60 : 0;
      secKnown = (n == 3);
      clkRef = millis();
      clockOk = true;
    }
  } else if (type == 'J') {
    strncpy(dateStr, data, sizeof(dateStr) - 1);
    dateStr[sizeof(dateStr) - 1] = '\0';
  } else if (type == 'W') {
    strncpy(wdayStr, data, sizeof(wdayStr) - 1);
    wdayStr[sizeof(wdayStr) - 1] = '\0';
  }
}

void readSerial() {
  while (Serial.available()) {
    char c = Serial.read();
    if (c == '\n') {
      processLine();
    } else if (c != '\r') {
      if (lineLen < sizeof(line) - 1) {
        line[lineLen++] = c;
      }
    }
  }
}

// ======================= Clock / DHT =======================

void getClock(int &h, int &m, int &s) {
  unsigned long tot = (unsigned long)clkH * 3600UL + clkM * 60UL + clkS
                      + (millis() - clkRef) / 1000UL;
  tot %= 86400UL;
  h = tot / 3600;
  m = (tot % 3600) / 60;
  s = tot % 60;
}

void updateDht() {
  if (!isIdle()) return;   // only read while idle (keeps serial responsive during music)
  if (dhtTried && millis() - lastDht < DHT_INTERVAL) return;
  dhtTried = true;
  lastDht = millis();

#if USE_DHT
  float t = dht.readTemperature();
  float h = dht.readHumidity();
  if (!isnan(t) && !isnan(h)) {
    tempC = (int)(t + 0.5);
    hum = (int)(h + 0.5);
    dhtOk = true;
  }
#endif
}

// ======================= LED =======================

int gammaOut(float v, float cal) {
  float x = constrain(v, 0.0, 255.0) / 255.0;
  int s = (int)(pow(x, GAMMA) * 255.0 * cal + 0.5);
  return constrain(s, 0, 255);
}

void updateLed() {
  if (millis() - lastFade < 15) return;
  lastFade = millis();

  bool idleLed = IDLE_LED && isIdle();
  byte tr = idleLed ? IDLE_R : targetR;
  byte tg = idleLed ? IDLE_G : targetG;
  byte tb = idleLed ? IDLE_B : targetB;

  // Target brightness: pause and ambient light (with smooth fade)
  float targetBrightness = playing ? 1.0 : PAUSED_BRIGHTNESS;
  if (idleLed) {
    targetBrightness = 0.03 + 0.12 * (0.5 + 0.5 * sin(millis() / 1200.0));  // breathing
  }
  if (USE_LDR) {
    int v = analogRead(ldr);
    int light = constrain(map(v, 100, 900, 100, 0), 0, 100);
    targetBrightness *= 0.2 + 0.8 * light / 100.0;
  }
  brightness += (targetBrightness - brightness) * 0.12;

  // Smooth color fade
  curR += (tr - curR) * 0.12;
  curG += (tg - curG) * 0.12;
  curB += (tb - curB) * 0.12;

  // Beat flash decays quickly
  flash *= 0.88;
  if (flash < 0.01) flash = 0;

  float beatMul = 1.0;
  if (beatOn) {
    // Use the S (playing) flag when it is reliable, but also treat a recent
    // "B" as proof that music is playing. Then a lost S line (serial buffer
    // full during an OLED redraw) cannot leave the LED stuck without pulsing.
    bool beatActive = playing || (millis() - lastBeatMs < 2000UL);
    if (beatActive) beatMul = BEAT_MIN + (1.0 - BEAT_MIN) * flash;
  }
  float mult = brightness * beatMul * MAX_BRIGHTNESS;

  analogWrite(pinRed,   (int)(gammaOut(curR, CAL_R) * mult));
  analogWrite(pinGreen, (int)(gammaOut(curG, CAL_G) * mult));
  analogWrite(pinBlue,  (int)(gammaOut(curB, CAL_B) * mult));
}

// ======================= OLED =======================

void scrollingText(const char *txt, int y, byte size) {
  int width = strlen(txt) * 6 * size;
  oled.setTextSize(size);
  if (width <= 128) {
    oled.setCursor(0, y);
    oled.print(txt);
    return;
  }
  int cycle = width + 30;
  int x = -(int)(((millis() - scrollStart) / 30) % cycle);
  oled.setCursor(x, y);
  oled.print(txt);
  oled.setCursor(x + cycle, y);
  oled.print(txt);
}

long currentPos() {
  long p = basePos;
  if (playing) p += (millis() - posRef) / 1000;
  if (duration > 0) {
    if (p > duration) p = duration;
  }
  return p;
}

void timeText(long s, char *out) {
  sprintf_P(out, PSTR("%ld:%02ld"), s / 60, s % 60);
}

void printCentered(const char *txt, int y) {
  int w = strlen(txt) * 6;
  oled.setCursor((128 - w) / 2, y);
  oled.print(txt);
}

void drawIdle() {
  oled.clearDisplay();
  oled.setTextSize(1);

  int h, mi, s;
  bool ok = clockOk;
  if (ok) getClock(h, mi, s);

  // Big clock
  char buf[24];
  oled.setTextSize(3);
  oled.setCursor(2, 0);
  if (ok) {
    sprintf_P(buf, PSTR("%02d:%02d"), h, mi);
    if (s % 2) buf[2] = ' ';   // blinking colon
    oled.print(buf);
    if (secKnown) {
      oled.setTextSize(2);
      oled.setCursor(96, 10);
      sprintf_P(buf, PSTR("%02d"), s);
      oled.print(buf);
    }
  } else {
    oled.print(F("--:--"));
  }

  // Date
  oled.setTextSize(1);
  if (dateStr[0]) {
    if (wdayStr[0]) sprintf_P(buf, PSTR("%s, %s"), wdayStr, dateStr);
    else strcpy(buf, dateStr);
    printCentered(buf, 28);
  } else {
    oled.setCursor(1, 28);
    oled.print(F("Waiting for time sync"));
  }

  oled.drawFastHLine(0, 38, 128, WHITE);

  // Temperature / humidity
  oled.setCursor(10, 42);
  if (dhtOk) {
    oled.print(F("T:"));
    oled.print(tempC);
    oled.write(247);          // degree symbol
    oled.print(F("C"));
    oled.setCursor(76, 42);
    oled.print(F("H:"));
    oled.print(hum);
    oled.print(F("%"));
  } else {
    oled.print(F("T:--  "));
    oled.setCursor(76, 42);
    oled.print(F("H:--"));
  }

  // Waiting message with animated dots
  byte dots = (millis() / 500) % 4;
  oled.setCursor(((128 - 17 * 6) / 2), 55);   // fixed x so the text doesn't jump
  oled.print(F("Waiting for music"));
  for (byte i = 0; i < dots; i++) oled.print('.');

  readSerial();      // drain commands before blocking ~25 ms on the I2C push
  oled.display();
}

void drawScreen() {
  if (isIdle()) {
    drawIdle();
    return;
  }

  oled.clearDisplay();

  oled.setTextSize(1);
  oled.setCursor(0, 0);
  oled.print(playing ? F("> Playing") : F("|| Paused"));

  // Beat mode indicator: a "B" that blinks with the beats
  if (beatOn) {
    bool lit = (flash > 0.5);
    if (lit) oled.fillRect(66, 0, 8, 9, WHITE);
    oled.setTextColor(lit ? BLACK : WHITE);
    oled.setCursor(67, 1);
    oled.print('B');
    oled.setTextColor(WHITE);
  }

  char hex[8];
  sprintf_P(hex, PSTR("#%02X%02X%02X"), targetR, targetG, targetB);
  oled.setCursor(78, 0);
  oled.print(hex);
  oled.drawFastHLine(0, 10, 128, WHITE);

  scrollingText(title, 14, 2);
  scrollingText(artist, 34, 1);

  char t1[12], t2[12];
  oled.drawRect(0, 46, 128, 6, WHITE);
  if (duration > 0) {
    long p = currentPos();
    oled.fillRect(0, 46, (p * 128L) / duration, 6, WHITE);
    timeText(p, t1);
    timeText(duration, t2);
  } else {
    strcpy_P(t1, PSTR("--:--"));
    strcpy_P(t2, PSTR("--:--"));
  }
  oled.setTextSize(1);
  oled.setCursor(0, 56);
  oled.print(t1);
  oled.setCursor(128 - strlen(t2) * 6, 56);
  oled.print(t2);

  readSerial();      // drain commands before blocking ~25 ms on the I2C push
  oled.display();
}