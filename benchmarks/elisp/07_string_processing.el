;;; 07_string_processing.el --- String processing benchmark -*- lexical-binding: t; -*-
(setq max-lisp-eval-depth 50000)
(require 'subr-x)

(defun bench-run-string (lines)
  (let ((total-latency 0)
        (total-bytes 0)
        (matched-ok 0)
        (i 0))
    (while (< i lines)
      (let* ((lat (% (+ (* i 37) 13) 997))
             (code (if (= (% i 5) 0) "500" "200"))
             (line (concat "2026-11-17T12:00:00Z service=gateway req="
                           (number-to-string i)
                           " latency="
                           (number-to-string lat)
                           "ms status="
                           code)))
        (setq total-bytes (+ total-bytes (string-bytes line)))
        (when (string-suffix-p "status=200" line)
          (setq matched-ok (1+ matched-ok)))
        (let ((pos (string-search "latency=" line)))
          (when pos
            (let* ((start (+ pos 8))
                   (end-pos (string-search "ms status=" line)))
              (when end-pos
                (setq total-latency
                      (+ total-latency
                         (string-to-number (substring line start end-pos)))))))))
      (setq i (1+ i)))
    (+ (* total-latency 100) matched-ok (% total-bytes 100000))))

(defun bench-main ()
  (let* ((t0 (float-time))
         (ans (bench-run-string 25000))
         (ms (* (- (float-time) t0) 1000.0)))
    (princ (format "KERNEL_MS:%.6f\nCHECKSUM:%d\n" ms ans))))
