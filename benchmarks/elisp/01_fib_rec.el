;;; 01_fib_rec.el --- Recursive Fibonacci benchmark -*- lexical-binding: t; -*-
(setq max-lisp-eval-depth 50000)

(defun bench-fib (n)
  (if (< n 2)
      n
    (+ (bench-fib (- n 1))
       (bench-fib (- n 2)))))

(defun bench-main ()
  (let* ((t0 (float-time))
         (ans (bench-fib 34))
         (ms (* (- (float-time) t0) 1000.0)))
    (princ (format "KERNEL_MS:%.6f\nCHECKSUM:%d\n" ms ans))))
